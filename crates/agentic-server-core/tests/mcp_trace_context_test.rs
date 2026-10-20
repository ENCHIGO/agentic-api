//! A gateway MCP `tools/call` carries the calling span's trace context and baggage
//! to the tool server, both as HTTP headers and in `params._meta` (#460).
//!
//! One test per binary: the global subscriber and propagator are process-wide.

use std::sync::{Arc, Mutex};

use agentic_core::tool::McpClient;
use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry::{Context, KeyValue, baggage::BaggageExt as _};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::{Value, json};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

#[derive(Debug, Default)]
struct Seen {
    headers: Option<HeaderMap>,
    meta: Option<Value>,
}

/// Stateless streamable-HTTP MCP server recording the `tools/call` request.
async fn spawn_recording_mcp() -> (String, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let recorder = Arc::clone(&seen);
    let app = Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, axum::Json(request): axum::Json<Value>| {
            let recorder = Arc::clone(&recorder);
            async move {
                let id = request["id"].clone();
                if id.is_null() {
                    return StatusCode::ACCEPTED.into_response();
                }
                let result = match request["method"].as_str() {
                    Some("initialize") => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                        "serverInfo": {"name": "stub", "version": "0"}}),
                    Some("tools/call") => {
                        let mut seen = recorder.lock().unwrap();
                        seen.headers = Some(headers);
                        seen.meta = Some(request["params"]["_meta"].clone());
                        json!({"content": [{"type": "text", "text": "ok"}], "isError": false})
                    }
                    _ => json!({}),
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/mcp"), seen)
}

fn trace_id_of(traceparent: &str) -> &str {
    traceparent.split('-').nth(1).expect("traceparent has a trace id")
}

#[tokio::test]
async fn mcp_tool_calls_carry_the_calling_trace_and_baggage() {
    // Same propagators the server installs.
    opentelemetry::global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]));
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(InMemorySpanExporter::default())
        .build();
    let layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("test"));
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer)).unwrap();

    let (url, seen) = spawn_recording_mcp().await;
    let client = McpClient::connect(&url, None).await.expect("connect");

    // A request whose caller sent `baggage: tenant=acme`.
    let root = tracing::info_span!("test.root");
    let _ = root.set_parent(Context::current().with_baggage(vec![KeyValue::new("tenant", "acme")]));
    let trace_id = format!("{}", root.context().span().span_context().trace_id());
    client
        .call_tool("echo", Some(json!({})))
        .instrument(root)
        .await
        .expect("tools/call");

    let seen = seen.lock().unwrap();
    let headers = seen.headers.as_ref().expect("tools/call reached the server");
    let traceparent = headers
        .get("traceparent")
        .expect("traceparent header")
        .to_str()
        .unwrap();
    assert_eq!(
        trace_id_of(traceparent),
        trace_id,
        "the tool call joins the caller's trace"
    );
    assert_eq!(headers.get("baggage").expect("baggage header"), "tenant=acme");
    let meta = seen.meta.as_ref().unwrap();
    assert_eq!(
        meta["traceparent"], traceparent,
        "_meta carries the same context for non-HTTP transports"
    );
    assert_eq!(meta["baggage"], "tenant=acme");
}
