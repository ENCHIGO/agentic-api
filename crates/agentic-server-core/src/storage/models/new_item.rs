//! Items prepared for insertion, and the rows written for them.

use super::item::Item;
use crate::storage::{StorageError, StoreResult};
use crate::types::io::InOutItem;
use crate::utils::common::uuid7_str;

/// Distinguishes client-supplied conversation items from persisted model history.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemSource {
    ConversationApi,
    ResponseHistory,
}

/// Whether an item was supplied as input or produced as model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRole {
    Input,
    Output,
}

/// An item serialized for insertion.
#[derive(Debug)]
pub struct NewItem {
    /// Public item ID: the supplied one, or one generated for the item's kind.
    pub id: String,
    pub data: String,
    pub role: ItemRole,
}

/// The row written for a [`NewItem`], keeping that item's role.
///
/// The row ID identifies this stored occurrence; the public ID it carries may be
/// shared by other occurrences of the same item.
#[derive(Debug)]
pub struct WrittenItem {
    pub row: Item,
    pub role: ItemRole,
}

impl WrittenItem {
    /// Row IDs of the written output occurrences, in write order.
    pub(crate) fn output_row_ids(written: &[Self]) -> Vec<String> {
        written
            .iter()
            .filter(|item| item.role == ItemRole::Output)
            .map(|item| item.row.id.clone())
            .collect()
    }
}

/// Serialize new history items, retaining public IDs already assigned to input or output items.
/// Items without IDs receive a storage-generated ID.
///
/// # Errors
/// Returns an error if serialization fails or a supplied ID is empty or has an invalid prefix.
pub(crate) fn serialize_new_items(items: Vec<InOutItem>, source: ItemSource) -> StoreResult<Vec<NewItem>> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let (existing_id, prefix, role) = match &item {
                InOutItem::Input(input) => (input.id(), input.id_prefix(), ItemRole::Input),
                InOutItem::Output(output) => (output.id(), output.id_prefix(), ItemRole::Output),
            };
            // Upstream model IDs are preserved; some vLLM reasoning items use
            // msg_ IDs. Prefix errors apply to supplied Conversation Items IDs.
            if source == ItemSource::ConversationApi
                && let (Some(id), Some(prefix)) = (existing_id, prefix)
                && !id.starts_with(prefix)
            {
                return Err(StorageError::InvalidItemId {
                    param: format!("items[{index}].id"),
                    id: id.to_owned(),
                    prefix: prefix.trim_end_matches('_'),
                });
            }
            if existing_id == Some("") {
                return Err(StorageError::Validation("item id cannot be an empty string".to_owned()));
            }
            let id = existing_id.map_or_else(|| uuid7_str(prefix.unwrap_or("item_")), str::to_owned);
            let data = String::try_from(&item)?;
            Ok(NewItem { id, data, role })
        })
        .collect()
}
