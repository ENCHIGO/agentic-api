//! Message-content validation for typed Responses execution.

use std::fmt;

use thiserror::Error;

use super::{InputContent, InputItem, InputMessageContent, ResponsesInput};
use crate::types::io::{InOutItem, OutputItem};

/// Invalid message content, with its position in the request history.
#[derive(Debug, Error)]
pub struct InputValidationError {
    item_index: usize,
    #[source]
    source: MessageContentError,
}

impl fmt::Display for InputValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "input[{}].content", self.item_index)?;
        if let MessageContentError::UnsupportedPart { part_index, .. } = &self.source {
            write!(f, "[{part_index}]")?;
        }
        write!(f, ": {}", self.source)
    }
}

#[derive(Debug, Error)]
enum MessageContentError {
    #[error("a message must contain at least one content part")]
    Empty,
    #[error(
        "message content part type `{kind}` is not supported by the typed Responses executor; \
         provide input_text or input_image content instead"
    )]
    UnsupportedPart { part_index: usize, kind: String },
}

impl InputMessageContent {
    fn validate(&self) -> Result<(), MessageContentError> {
        match self {
            Self::Text(_) => Ok(()),
            Self::Parts(parts) if parts.is_empty() => Err(MessageContentError::Empty),
            Self::Parts(parts) => parts.iter().enumerate().try_for_each(|(part_index, part)| {
                let kind = match part {
                    InputContent::InputFile(_) => "input_file",
                    InputContent::Unknown(kind) => kind,
                    InputContent::InputText(_)
                    | InputContent::InputImage(_)
                    | InputContent::OutputText(_)
                    | InputContent::Refusal(_)
                    | InputContent::ReasoningText(_) => return Ok(()),
                };
                Err(MessageContentError::UnsupportedPart {
                    part_index,
                    kind: kind.to_owned(),
                })
            }),
        }
    }
}

impl InputItem {
    fn validate(&self) -> Result<(), MessageContentError> {
        match self {
            Self::Message(message) => message.content.validate(),
            _ => Ok(()),
        }
    }
}

impl InOutItem {
    fn validate(&self) -> Result<(), MessageContentError> {
        match self {
            Self::Input(item) => item.validate(),
            Self::Output(OutputItem::Message(message)) if message.content.is_empty() => Err(MessageContentError::Empty),
            Self::Output(_) => Ok(()),
        }
    }
}

impl ResponsesInput {
    /// Validate message content before typed execution or upstream construction.
    ///
    /// Run after routing: raw-proxy requests preserve their original bytes and
    /// leave content support decisions to the upstream. Restored history must
    /// pass the same validation. Structured tool outputs have their own content
    /// contract and can contain files.
    ///
    /// # Errors
    /// Returns the item and content-part location of an empty message array or
    /// a message content type the typed executor cannot convey.
    pub fn validate(&self) -> Result<(), InputValidationError> {
        match self {
            Self::Text(_) => Ok(()),
            Self::Items(items) => items.iter().enumerate().try_for_each(|(item_index, item)| {
                item.validate()
                    .map_err(|source| InputValidationError { item_index, source })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::event::MessageStatus;
    use crate::types::io::{OutputMessage, OutputTextContent};
    use serde_json::json;

    #[test]
    fn validation_preserves_original_item_and_content_positions() {
        for kind in ["input_file", "future_content"] {
            let input: ResponsesInput = serde_json::from_value(json!([
                {"type":"function_call_output", "call_id":"call_1", "output":"done"},
                {"role":"user", "content":[{"type":"input_text", "text":"hello"}, {"type":kind}]}
            ]))
            .unwrap();
            let error = input.validate().unwrap_err();
            assert_eq!(error.item_index, 1);
            assert!(
                matches!(&error.source, MessageContentError::UnsupportedPart { part_index: 1, kind: actual } if actual == kind)
            );
            assert!(error.to_string().starts_with("input[1].content[1]:"));
        }
    }

    #[test]
    fn output_messages_are_validated_without_restricting_tool_results() {
        let mut message = OutputMessage::new("msg_1", MessageStatus::Completed);
        let input = ResponsesInput::from(vec![InOutItem::Output(OutputItem::Message(message.clone()))]);
        let error = input.validate().unwrap_err();
        assert!(matches!(error.source, MessageContentError::Empty));
        assert_eq!(
            error.to_string(),
            "input[0].content: a message must contain at least one content part"
        );

        message.content.push(OutputTextContent::new("answer").into());
        ResponsesInput::from(vec![InOutItem::Output(OutputItem::Message(message))])
            .validate()
            .unwrap();
        let tool_result: ResponsesInput = serde_json::from_value(json!([
            {"type":"function_call_output", "call_id":"call_1", "output":[
                {"type":"input_file", "file_id":"file_1"}
            ]}
        ]))
        .unwrap();
        tool_result.validate().unwrap();
    }
}
