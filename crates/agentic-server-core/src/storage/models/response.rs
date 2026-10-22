//! LLM API response stored in the database.

use super::super::pool::{DbPool, DbResult, DbTransaction};
use crate::utils::common::utcnow_str;

/// LLM API response stored in the database.
///
/// Maps to the `responses` table and represents a single API response
/// with its metadata and history chain.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Response {
    /// Unique response identifier.
    pub id: String,

    /// Optional conversation this response belongs to.
    pub conversation_id: Option<String>,

    /// Optional reference to previous response for chaining.
    pub previous_response_id: Option<String>,

    /// History item IDs as JSON array string.
    pub history_item_ids: Option<String>,

    /// Response metadata as JSON object string.
    pub metadata: Option<String>,

    /// Creation timestamp as Unix timestamp in seconds.
    pub created_at: i64,
}

/// Create a response in a transaction and return it.
///
/// # Errors
/// Returns `DbResult::Err` if the database insertion fails.
pub async fn create_in_tx(
    tx: &mut DbTransaction<'_>,
    id: &str,
    conversation_id: Option<&str>,
    previous_response_id: Option<&str>,
    history_item_ids: Option<&str>,
    metadata: Option<&str>,
) -> DbResult<Response> {
    let now = utcnow_str();
    sqlx::query_as::<_, Response>(
        "INSERT INTO responses \
         (id, conversation_id, previous_response_id, history_item_ids, metadata, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
    )
    .bind(id)
    .bind(conversation_id)
    .bind(previous_response_id)
    .bind(history_item_ids)
    .bind(metadata)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
}

/// Get a response by ID.
///
/// # Errors
/// Returns `DbResult::Err` if the database query fails.
pub async fn get(pool: &DbPool, id: &str) -> DbResult<Option<Response>> {
    sqlx::query_as::<_, Response>("SELECT * FROM responses WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// History items checked per query when looking for references that keep them alive.
const REFERENCE_CHECK_BATCH: usize = 64;

/// Read a response row inside a transaction.
///
/// # Errors
/// Returns `DbResult::Err` if the query fails.
pub async fn get_in_tx(tx: &mut DbTransaction<'_>, id: &str) -> DbResult<Option<Response>> {
    sqlx::query_as::<_, Response>("SELECT * FROM responses WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
}

/// Delete a response row.
///
/// Foreign keys clear `responses.previous_response_id` and
/// `conversations.latest_response_id` references to it.
///
/// # Errors
/// Returns `DbResult::Err` if the database statement fails.
pub async fn delete_in_tx(tx: &mut DbTransaction<'_>, id: &str) -> DbResult<()> {
    sqlx::query("DELETE FROM responses WHERE id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Point a conversation that named `deleted_id` as its latest response at its newest remaining response.
///
/// The latest response carries the conversation's state (effective settings, tools loaded by
/// tool search, the multi-agent tree), so the conversation keeps the closest state it still has.
///
/// # Errors
/// Returns `DbResult::Err` if a database statement fails.
pub async fn repoint_latest_in_tx(tx: &mut DbTransaction<'_>, conversation_id: &str, deleted_id: &str) -> DbResult<()> {
    sqlx::query(
        "UPDATE conversations SET latest_response_id = ( \
             SELECT id FROM responses WHERE conversation_id = $1 AND id <> $2 \
             ORDER BY created_at DESC, id DESC LIMIT 1 \
         ) WHERE id = $1 AND latest_response_id = $2",
    )
    .bind(conversation_id)
    .bind(deleted_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Delete the history rows of a deleted response that nothing else keeps.
///
/// Each batch scans the `responses` table once; deletion is rare enough to pay that.
///
/// A row stays when it belongs to a conversation, when a conversation item references it,
/// or when another response's history lists it (descendants and siblings copy inherited
/// history). Call after the response row itself is deleted. Returns the number of rows removed.
///
/// # Errors
/// Returns `DbResult::Err` if a database statement fails.
pub async fn delete_unreferenced_items_in_tx(tx: &mut DbTransaction<'_>, history_item_ids: &[String]) -> DbResult<u64> {
    let mut removed = 0;
    for batch in history_item_ids.chunks(REFERENCE_CHECK_BATCH) {
        let placeholders = (1..=batch.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let detached = format!(
            "SELECT id FROM items WHERE id IN ({placeholders}) AND conversation_id IS NULL \
             AND NOT EXISTS (SELECT 1 FROM items referencing WHERE referencing.reference_id = items.id)"
        );
        let mut query = sqlx::query_scalar::<_, String>(&detached);
        for id in batch {
            query = query.bind(id);
        }
        let mut candidates: Vec<String> = query.fetch_all(&mut **tx).await?;
        if candidates.is_empty() {
            continue;
        }

        // `LIKE` narrows the scan; parsing confirms an exact entry (`_` in IDs is a wildcard).
        let patterns = (1..=candidates.len())
            .map(|index| format!("history_item_ids LIKE ${index}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!("SELECT history_item_ids FROM responses WHERE {patterns}");
        let mut query = sqlx::query_scalar::<_, Option<String>>(&sql);
        for id in &candidates {
            query = query.bind(format!("%\"{id}\"%"));
        }
        for history in query.fetch_all(&mut **tx).await?.into_iter().flatten() {
            let listed: Vec<String> = serde_json::from_str(&history).unwrap_or_default();
            candidates.retain(|id| !listed.contains(id));
        }
        if candidates.is_empty() {
            continue;
        }

        let placeholders = (1..=candidates.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let delete = format!("DELETE FROM items WHERE id IN ({placeholders})");
        let mut query = sqlx::query(&delete);
        for id in &candidates {
            query = query.bind(id);
        }
        removed += query.execute(&mut **tx).await?.rows_affected();
    }
    Ok(removed)
}

/// Get the response that committed a conversation turn.
///
/// # Errors
/// Returns `DbResult::Err` if the database query fails.
pub async fn get_conversation_turn(
    pool: &DbPool,
    conversation_id: &str,
    response_id: &str,
) -> DbResult<Option<Response>> {
    sqlx::query_as::<_, Response>(
        "SELECT * FROM responses \
         WHERE id = $1 AND conversation_id = $2",
    )
    .bind(response_id)
    .bind(conversation_id)
    .fetch_optional(pool)
    .await
}

impl Response {
    /// Deserialize history references, treating only SQL NULL as legacy empty history.
    ///
    /// # Errors
    /// Returns an error for malformed JSON or a value other than an array of strings.
    pub fn history_item_ids_vec(&self) -> Result<Vec<String>, serde_json::Error> {
        self.history_item_ids
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map(Option::unwrap_or_default)
    }

    /// Deserialize metadata from JSON string to the given type.
    ///
    /// # Errors
    /// Returns an error if present metadata does not match the requested schema.
    pub fn metadata_as<T: serde::de::DeserializeOwned>(&self) -> Result<Option<T>, serde_json::Error> {
        self.metadata.as_deref().map(serde_json::from_str).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_history_ids_empty() {
        let response = Response {
            id: "test".to_string(),
            conversation_id: None,
            previous_response_id: None,
            history_item_ids: None,
            metadata: None,
            created_at: 1_704_067_200,
        };

        let ids = response.history_item_ids_vec().expect("legacy empty history");
        assert!(ids.is_empty());
    }

    #[test]
    fn test_response_history_ids_valid() {
        let response = Response {
            id: "test".to_string(),
            conversation_id: None,
            previous_response_id: None,
            history_item_ids: Some(r#"["item_1", "item_2"]"#.to_string()),
            metadata: None,
            created_at: 1_704_067_200,
        };

        let ids = response.history_item_ids_vec().expect("valid history references");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "item_1");
    }

    #[test]
    fn test_response_metadata_deserialize() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct TestMeta {
            model: String,
        }

        let response = Response {
            id: "resp_1".to_string(),
            conversation_id: None,
            previous_response_id: None,
            history_item_ids: None,
            metadata: Some(r#"{"model":"gpt-4"}"#.to_string()),
            created_at: 1_704_067_200,
        };

        let meta: Option<TestMeta> = response.metadata_as().expect("valid metadata");
        assert!(meta.is_some());
    }
}
