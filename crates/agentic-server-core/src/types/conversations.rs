//! Types for the OpenAI-compatible Conversations API.

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize, Serializer};

use super::event::MessageStatus;
use super::io::{InputContent, InputMessageContent, InputTextContent};
use serde_json::Value;

use super::io::{InOutItem, InputItem};

/// String metadata attached to a conversation.
pub type ConversationMetadata = BTreeMap<String, String>;

/// Request to create a new conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateConversationRequest {
    /// Optional metadata as a JSON object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ConversationMetadata>,

    /// Optional initial items to add to the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<InOutItem>>,
}

/// Request to update a conversation's metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateConversationRequest {
    /// Metadata as a JSON object.
    pub metadata: ConversationMetadata,
}

/// Response for a conversation resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConversationResponse {
    /// Unique conversation identifier.
    pub id: String,

    /// Object type, always "conversation".
    pub object: String,

    /// Creation timestamp as Unix timestamp in seconds.
    pub created_at: i64,

    /// Metadata as a JSON object.
    #[serde(default)]
    pub metadata: ConversationMetadata,
}

/// Request to append up to 20 items to a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateItemRequest {
    /// Items to append in the supplied order.
    pub items: Vec<InOutItem>,
}

/// Response for listing items with pagination.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListItemsResponse {
    /// Object type, always "list".
    pub object: String,

    /// Array of conversation items.
    pub data: Vec<ItemResponse>,

    /// Whether there are more items available.
    pub has_more: bool,

    /// ID of the first item in this page.
    pub first_id: Option<String>,

    /// ID of the last item in this page.
    pub last_id: Option<String>,
}

/// A conversation item with its storage identity and flattened public payload.
#[derive(Debug, Clone)]
pub struct ItemResponse {
    pub id: String,
    pub item: InOutItem,
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for ItemResponse {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{AllOfBuilder, Type};
        use utoipa::openapi::{ObjectBuilder, Ref};
        AllOfBuilder::new()
            .item(
                ObjectBuilder::new()
                    .property("id", ObjectBuilder::new().schema_type(Type::String))
                    .required("id"),
            )
            .item(Ref::from_schema_name("InOutItem"))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for ItemResponse {}

impl Serialize for ItemResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.item).map_err(serde::ser::Error::custom)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::ser::Error::custom("item must be an object"))?;
        // Stored Responses output may retain an upstream ID. The public resource
        // must use the same ID as its retrieval URL and pagination cursor, once.
        object.insert("id".to_owned(), Value::String(self.id.clone()));
        value.serialize(serializer)
    }
}

/// Ordering for conversation item pages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ItemOrder {
    Asc,
    #[default]
    Desc,
}

/// Page size for item listings, validated once to `1..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLimit(u8);

impl ItemLimit {
    const MAX: u8 = 100;

    /// The number of items a page holds.
    #[must_use]
    pub fn get(self) -> usize {
        usize::from(self.0)
    }

    /// One more than the page size: fetching it tells whether another page follows.
    #[must_use]
    pub fn probe(self) -> i64 {
        i64::from(self.0) + 1
    }
}

impl Default for ItemLimit {
    fn default() -> Self {
        Self(20)
    }
}

impl TryFrom<i64> for ItemLimit {
    type Error = InvalidItemLimit;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        u8::try_from(value)
            .ok()
            .filter(|limit| (1..=Self::MAX).contains(limit))
            .map(Self)
            .ok_or(InvalidItemLimit)
    }
}

/// A page size outside `1..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("limit must be between 1 and 100")]
pub struct InvalidItemLimit;

/// Validated pagination for item listings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemPage<'a> {
    pub limit: ItemLimit,
    /// Public ID of the last item on the previous page.
    pub after: Option<&'a str>,
    pub order: ItemOrder,
}

/// Extra item fields a listing can be asked to include, as `OpenAI` defines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ItemInclude {
    #[serde(rename = "file_search_call.results")]
    FileSearchCallResults,
    #[serde(rename = "web_search_call.results")]
    WebSearchCallResults,
    #[serde(rename = "web_search_call.action.sources")]
    WebSearchCallActionSources,
    #[serde(rename = "message.input_image.image_url")]
    MessageInputImageUrl,
    #[serde(rename = "computer_call_output.output.image_url")]
    ComputerCallOutputImageUrl,
    #[serde(rename = "code_interpreter_call.outputs")]
    CodeInterpreterCallOutputs,
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
    #[serde(rename = "message.output_text.logprobs")]
    MessageOutputTextLogprobs,
}

impl FromStr for ItemInclude {
    type Err = serde::de::value::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::deserialize(value.into_deserializer())
    }
}

impl ItemInclude {
    /// Whether stored items already carry this field.
    ///
    /// Stored items keep every field their item had, so expansions that read the
    /// item itself are served as stored. Hosted tool results come from data sources
    /// the gateway does not keep, so those expansions cannot be served.
    #[must_use]
    pub const fn is_stored_with_items(self) -> bool {
        match self {
            Self::MessageInputImageUrl
            | Self::ComputerCallOutputImageUrl
            | Self::ReasoningEncryptedContent
            | Self::MessageOutputTextLogprobs => true,
            Self::FileSearchCallResults
            | Self::WebSearchCallResults
            | Self::WebSearchCallActionSources
            | Self::CodeInterpreterCallOutputs => false,
        }
    }
}

/// Response for successful deletion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeletedResponse {
    /// ID of the deleted resource.
    pub id: String,

    /// Object type.
    pub object: String,

    /// Whether the deletion was successful.
    pub deleted: bool,
}

impl ConversationResponse {
    /// Create a new conversation response.
    #[must_use]
    pub fn new(id: String, created_at: i64, metadata: Option<ConversationMetadata>) -> Self {
        Self {
            id,
            object: "conversation".to_string(),
            created_at,
            metadata: metadata.unwrap_or_default(),
        }
    }
}

impl ItemResponse {
    /// Build the public resource, expanding shorthand message content.
    #[must_use]
    pub fn new(id: String, mut item: InOutItem) -> Self {
        if let InOutItem::Input(InputItem::Message(message)) = &mut item {
            message.status.get_or_insert(MessageStatus::Completed);
            if let InputMessageContent::Text(text) = &mut message.content {
                let content = InputTextContent::new(std::mem::take(text));
                message.content = InputMessageContent::Parts(vec![InputContent::InputText(content)]);
            }
        }
        Self { id, item }
    }
}

impl ListItemsResponse {
    /// Create a new list response.
    #[must_use]
    pub fn new(data: Vec<ItemResponse>, has_more: bool) -> Self {
        let first_id = data.first().map(|item| item.id.clone());
        let last_id = data.last().map(|item| item.id.clone());

        Self {
            object: "list".to_string(),
            data,
            has_more,
            first_id,
            last_id,
        }
    }
}

impl DeletedResponse {
    /// Create a deleted conversation response.
    #[must_use]
    pub fn conversation(id: String) -> Self {
        Self {
            id,
            object: "conversation.deleted".to_string(),
            deleted: true,
        }
    }

    /// Create a deleted response object, matching the `DELETE /v1/responses/{id}` endpoint.
    #[must_use]
    pub fn response(id: String) -> Self {
        Self {
            id,
            object: "response.deleted".to_string(),
            deleted: true,
        }
    }

    /// Create a deleted item response.
    #[must_use]
    pub fn item(id: String) -> Self {
        Self {
            id,
            object: "conversation.item.deleted".to_string(),
            deleted: true,
        }
    }
}

#[cfg(test)]
mod tests;
