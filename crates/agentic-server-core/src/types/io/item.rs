//! Shared input/output history items for Responses, Conversations, and storage.

use std::borrow::Cow;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{InputItem, OutputItem};
use crate::utils::common::serialize_to_value;

/// An input or output item retained in request, response, or conversation history.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum InOutItem {
    Input(InputItem),
    Output(OutputItem),
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for InOutItem {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::{Ref, schema::AnyOfBuilder};
        // Input and output shapes overlap, so a valid replay may match both.
        AnyOfBuilder::new()
            .item(Ref::from_schema_name("InputItem"))
            .item(Ref::from_schema_name("OutputItem"))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for InOutItem {}

impl PartialEq for InOutItem {
    fn eq(&self, other: &Self) -> bool {
        if std::mem::discriminant(self) != std::mem::discriminant(other) {
            return false;
        }
        match (serialize_to_value(self), serialize_to_value(other)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}

impl From<InputItem> for InOutItem {
    fn from(item: InputItem) -> Self {
        Self::Input(item)
    }
}

impl From<OutputItem> for InOutItem {
    fn from(item: OutputItem) -> Self {
        Self::Output(item)
    }
}

impl<'de> Deserialize<'de> for InOutItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let input_error = match InputItem::deserialize(&value) {
            Ok(input) => return Ok(Self::Input(input)),
            Err(error) => error,
        };
        // Only output-only shapes may use the output decoder. Shared shapes must
        // satisfy input requirements: upstream output decoding permits partial calls.
        match OutputItem::deserialize(value).map_err(serde::de::Error::custom)? {
            output @ (OutputItem::WebSearchCall(_) | OutputItem::McpCall(_)) => Ok(Self::Output(output)),
            OutputItem::Message(_)
            | OutputItem::FunctionCall(_)
            | OutputItem::CodeInterpreterCall(_)
            | OutputItem::ToolSearchCall(_)
            | OutputItem::CustomToolCall(_)
            | OutputItem::ShellCall(_)
            | OutputItem::McpListTools(_)
            | OutputItem::Reasoning(_)
            | OutputItem::Compaction(_)
            | OutputItem::MultiAgentCall(_)
            | OutputItem::MultiAgentCallOutput(_)
            | OutputItem::AgentMessage(_)
            | OutputItem::Unknown => Err(serde::de::Error::custom(input_error)),
        }
    }
}

impl InOutItem {
    /// Borrow an input record or project a public output using the existing continuation conversion.
    pub(crate) fn as_input(&self) -> Option<Cow<'_, InputItem>> {
        match self {
            Self::Input(input) => Some(Cow::Borrowed(input)),
            Self::Output(output) => output.to_input_item().map(Cow::Owned),
        }
    }

    /// Project replay inputs while preserving the public types of input tool calls.
    /// Output records follow the existing output-to-input continuation policy.
    pub(crate) fn to_replay_inputs(history: &[Self]) -> Vec<InputItem> {
        history.iter().filter_map(Self::as_input).map(Cow::into_owned).collect()
    }

    pub(crate) fn call_id(&self) -> Option<&str> {
        match self {
            Self::Input(item) => item.call_id(),
            Self::Output(OutputItem::FunctionCall(call)) => Some(&call.call_id),
            Self::Output(OutputItem::ToolSearchCall(call)) => Some(&call.call_id),
            Self::Output(OutputItem::CustomToolCall(call)) => Some(&call.call_id),
            Self::Output(OutputItem::ShellCall(call)) => Some(&call.call_id),
            Self::Output(_) => None,
        }
    }

    /// Converts stored history into input items for continuation processing.
    /// Internal items are removed later by `ResponsesInput::model_input`.
    #[must_use]
    pub fn into_input_items(history: Vec<InOutItem>) -> Vec<InputItem> {
        history
            .into_iter()
            .filter_map(|item| match item {
                Self::Input(item) => Some(item),
                Self::Output(output) => output.to_input_item(),
            })
            .map(|item| match item {
                InputItem::ShellCall(call) => InputItem::FunctionCall(call.into()),
                InputItem::ShellCallOutput(output) => InputItem::FunctionCallOutput(output.into()),
                InputItem::CustomToolCall(call) => InputItem::FunctionCall(call.into()),
                InputItem::CustomToolCallOutput(output) => InputItem::FunctionCallOutput(output.into()),
                item => item,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
