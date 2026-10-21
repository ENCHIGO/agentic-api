//! W3C trace context and baggage carried with MCP `tools/call` requests.
//!
//! MCP requests are sent by a transport task spawned when the pooled connection
//! was opened, so the HTTP layer cannot read the calling span from `Span::current()`.
//! The context is captured when the call is made and travels in the message's
//! `params._meta`: stdio servers read it there, and the HTTP transport restores it
//! and sends it as request headers.

use std::collections::HashMap;

use http::header::{HeaderName, HeaderValue};
use opentelemetry::propagation::Injector;
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, JsonRpcMessage, Meta};
use serde::{Deserialize, Serialize};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

const TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");
const TRACESTATE: HeaderName = HeaderName::from_static("tracestate");
const BAGGAGE: HeaderName = HeaderName::from_static("baggage");

/// The propagation fields an MCP call carries. Fields other propagators set are
/// dropped, so `_meta` never holds anything the HTTP transport would not forward.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct W3cPropagation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tracestate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    baggage: Option<String>,
}

impl Injector for W3cPropagation {
    fn set(&mut self, key: &str, value: String) {
        // The trace-context propagator writes an empty `tracestate` when there is none.
        if value.is_empty() {
            return;
        }
        match key {
            "traceparent" => self.traceparent = Some(value),
            "tracestate" => self.tracestate = Some(value),
            "baggage" => self.baggage = Some(value),
            _ => {}
        }
    }
}

impl W3cPropagation {
    /// The calling span's context, as the global propagator encodes it.
    pub(crate) fn capture() -> Self {
        let context = Span::current().context();
        let mut propagation = Self::default();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut propagation);
        });
        propagation
    }

    /// `_meta` for a `tools/call`, or `None` when there is nothing to propagate.
    pub(crate) fn into_meta(self) -> Option<Meta> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) if !map.is_empty() => Some(Meta(map)),
            _ => None,
        }
    }

    /// The context a `tools/call` carries in `_meta`; empty for any other message.
    pub(crate) fn from_message(message: &ClientJsonRpcMessage) -> Self {
        let JsonRpcMessage::Request(request) = message else {
            return Self::default();
        };
        let ClientRequest::CallToolRequest(call) = &request.request else {
            return Self::default();
        };
        let Some(meta) = call.params.meta.as_ref() else {
            return Self::default();
        };
        let fields = [TRACEPARENT, TRACESTATE, BAGGAGE].map(|name| {
            meta.0
                .get(name.as_str())
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        let [traceparent, tracestate, baggage] = fields;
        Self {
            traceparent,
            tracestate,
            baggage,
        }
    }

    /// Replace any configured propagation headers with this context.
    ///
    /// A new `traceparent` also drops a configured `tracestate`, which belongs to
    /// the trace being replaced. Values that are not valid header values (baggage
    /// properties may decode to control characters) are skipped, so telemetry never
    /// prevents the tool call.
    pub(crate) fn apply_to(self, headers: &mut HashMap<HeaderName, HeaderValue>) {
        let valid = |value: Option<String>| value.and_then(|value| HeaderValue::try_from(value).ok());
        if let Some(traceparent) = valid(self.traceparent) {
            headers.insert(TRACEPARENT, traceparent);
            headers.remove(&TRACESTATE);
        }
        if let Some(tracestate) = valid(self.tracestate) {
            headers.insert(TRACESTATE, tracestate);
        }
        if let Some(baggage) = valid(self.baggage) {
            headers.insert(BAGGAGE, baggage);
        }
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::{CallToolRequest, CallToolRequestParams, NumberOrString};

    use super::*;

    fn call_with(propagation: W3cPropagation) -> ClientJsonRpcMessage {
        let mut params = CallToolRequestParams::new("echo");
        params.meta = propagation.into_meta();
        ClientJsonRpcMessage::request(
            ClientRequest::CallToolRequest(CallToolRequest::new(params)),
            NumberOrString::Number(1),
        )
    }

    #[test]
    fn meta_round_trips_through_a_tools_call() {
        let propagation = W3cPropagation {
            traceparent: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_owned()),
            tracestate: None,
            baggage: Some("tenant=acme".to_owned()),
        };
        let restored = W3cPropagation::from_message(&call_with(propagation.clone()));
        assert_eq!(restored, propagation);
        assert!(W3cPropagation::default().into_meta().is_none());
    }

    #[test]
    fn replaces_configured_context_and_its_stale_tracestate() {
        let mut headers = HashMap::from([
            (TRACEPARENT, HeaderValue::from_static("00-stale-stale-01")),
            (TRACESTATE, HeaderValue::from_static("vendor=stale")),
            (BAGGAGE, HeaderValue::from_static("tenant=stale")),
            (HeaderName::from_static("x-api-key"), HeaderValue::from_static("kept")),
        ]);
        W3cPropagation {
            traceparent: Some("00-current-current-01".to_owned()),
            tracestate: None,
            baggage: Some("tenant=acme".to_owned()),
        }
        .apply_to(&mut headers);
        assert_eq!(headers[&TRACEPARENT], "00-current-current-01");
        assert!(!headers.contains_key(&TRACESTATE));
        assert_eq!(headers[&BAGGAGE], "tenant=acme");
        assert_eq!(headers[&HeaderName::from_static("x-api-key")], "kept");
    }

    #[test]
    fn skips_values_that_are_not_valid_headers() {
        let mut headers = HashMap::new();
        W3cPropagation {
            traceparent: Some("00-current-current-01".to_owned()),
            tracestate: None,
            baggage: Some("tenant=acme;note=\nfoo".to_owned()),
        }
        .apply_to(&mut headers);
        assert_eq!(headers[&TRACEPARENT], "00-current-current-01");
        assert!(!headers.contains_key(&BAGGAGE));
    }
}
