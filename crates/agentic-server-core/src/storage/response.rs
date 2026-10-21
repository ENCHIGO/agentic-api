//! Response storage operations and queries.

use std::collections::HashMap;
use std::convert::TryFrom;
use std::sync::Arc;

use super::models::{item, response};
use super::pool::DbPool;
use super::types::{ResponseData, ResponseMetadata, StorageError, StoreResult};
use crate::types::io::InOutItem;
use crate::utils::common::serialize_to_string;

/// Response storage operations.
#[derive(Clone, Debug)]
pub struct ResponseStore {
    pool: Option<Arc<DbPool>>,
}

impl ResponseStore {
    /// Creates a disabled response store (no persistence).
    ///
    /// Useful for testing or when response storage is not configured.
    #[must_use]
    pub fn disabled() -> Self {
        Self { pool: None }
    }

    /// Creates a new response store with database pool.
    ///
    /// # Arguments
    ///
    /// * `pool` - Connection pool for database access
    #[must_use]
    pub fn new(pool: Arc<DbPool>) -> Self {
        Self { pool: Some(pool) }
    }

    /// Returns a reference to the database pool.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotConfigured`] if store is disabled (no pool configured).
    fn pool(&self) -> StoreResult<&DbPool> {
        self.pool.as_deref().ok_or(StorageError::NotConfigured)
    }

    /// Retrieves a response by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the response is missing or invalid, the database query
    /// fails, or the store is disabled.
    pub async fn get(&self, response_id: &str) -> StoreResult<ResponseData> {
        let pool = self.pool()?;
        let row = response::get(pool, response_id)
            .await?
            .ok_or_else(|| StorageError::not_found("Response", response_id))?;
        row.try_into()
    }

    /// Deletes a stored response.
    ///
    /// Descendants created with `previous_response_id` stay readable because each
    /// one stores its own complete history item list; their reference to the
    /// deleted response is cleared by the foreign key.
    ///
    /// Deletion does not reach WebSocket sessions. A session that already caches this
    /// response as its latest checkpoint keeps continuing from it without a storage
    /// read; those sessions live per connection, possibly on another replica, and a
    /// storage check on that path would reorder automatic steering successors.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if no response has this ID, or an error
    /// if the database statement fails or the store is disabled.
    pub async fn delete(&self, response_id: &str) -> StoreResult<()> {
        if response::delete(self.pool()?, response_id).await? == 0 {
            return Err(StorageError::not_found("Response", response_id));
        }
        Ok(())
    }

    /// Rehydrates a response with full history.
    ///
    /// Fetches all history items referenced by a response.
    ///
    /// # Errors
    ///
    /// Returns an error if a history item is missing or invalid, the database query
    /// fails, or the store is disabled.
    pub async fn rehydrate(&self, response_id: &str) -> StoreResult<Vec<InOutItem>> {
        let response = self.get(response_id).await?;
        self.history_rows(&response)
            .await?
            .iter()
            .map(InOutItem::try_from)
            .collect()
    }

    /// Loads the stored rows of a response's history, oldest first.
    ///
    /// The history is the full context recorded when the response was stored:
    /// items inherited through `previous_response_id` or its conversation, this
    /// turn's input, and this turn's output.
    ///
    /// # Errors
    ///
    /// Returns an error if a history item is missing, the database query fails,
    /// or the store is disabled.
    pub(crate) async fn history_rows(&self, response: &ResponseData) -> StoreResult<Vec<item::Item>> {
        let rows = item::get_items(self.pool()?, &response.history_item_ids).await?;
        let mut rows_by_id: HashMap<String, item::Item> = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        response
            .history_item_ids
            .iter()
            .map(|id| {
                rows_by_id
                    .remove(id)
                    .ok_or_else(|| StorageError::InvalidHistoryItem { item_id: id.clone() })
            })
            .collect()
    }

    /// Persists a response with its items and metadata.
    ///
    /// Creates items and stores the associated response record.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if database operation fails or store is disabled.
    pub async fn persist(
        &self,
        response_id: &str,
        previous_response_id: Option<&str>,
        new_items: Vec<InOutItem>,
        metadata: &ResponseMetadata,
    ) -> StoreResult<()> {
        self.persist_with_conversation_id(response_id, None, previous_response_id, new_items, metadata)
            .await
    }

    /// Persists a response with its optional conversation association.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if database operation fails or store is disabled.
    pub(crate) async fn persist_with_conversation_id(
        &self,
        response_id: &str,
        conversation_id: Option<&str>,
        previous_response_id: Option<&str>,
        new_items: Vec<InOutItem>,
        metadata: &ResponseMetadata,
    ) -> StoreResult<()> {
        let pool = self.pool()?;

        let mut item_ids: Vec<String> = match previous_response_id {
            Some(prev_id) => self.get(prev_id).await?.history_item_ids,
            None => Vec::new(),
        };
        let items_ = item::serialize_new_items(new_items, item::ItemSource::ResponseHistory)?;

        let mut tx = pool.begin().await?;

        // History rows get their own row IDs, so repeated public IDs keep every position.
        let written = item::create_in_tx(&mut tx, items_, None).await?;
        let metadata_json = metadata.serialize_with_output_rows(&written)?;
        item_ids.extend(written.into_iter().map(|item| item.row.id));
        let history_item_ids_json = serialize_to_string(&item_ids)?;

        response::create_in_tx(
            &mut tx,
            response_id,
            conversation_id,
            previous_response_id,
            Some(&history_item_ids_json),
            Some(&metadata_json),
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::ResponseMetadata;
    use super::*;

    #[test]
    fn test_response_store_disabled() {
        let store = ResponseStore::disabled();
        assert!(store.pool().is_err());
    }

    #[test]
    fn test_response_metadata_default() {
        let meta = ResponseMetadata::default();
        assert!(meta.model.is_empty());
        assert!(meta.previous_response_id.is_none());
    }
}
