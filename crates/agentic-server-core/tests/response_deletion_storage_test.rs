//! Deleting a stored response erases the history rows nothing else keeps, and leaves a
//! conversation on its newest remaining response.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agentic_core::storage::models::conversation as conversation_model;
use agentic_core::storage::{ConversationStore, DbPool, ResponseMetadata, ResponseStore, create_pool_with_schema};
use agentic_core::types::io::InOutItem;
use serde_json::json;

static COUNTER: AtomicU64 = AtomicU64::new(0);

async fn pool() -> Arc<DbPool> {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let url = format!("sqlite:file:response_deletion_{id}?mode=memory&cache=shared");
    create_pool_with_schema(Some(&url)).await.unwrap()
}

fn message(text: &str) -> InOutItem {
    InOutItem::Input(serde_json::from_value(json!({"type": "message", "role": "user", "content": text})).unwrap())
}

fn answer(id: &str) -> InOutItem {
    InOutItem::Output(
        serde_json::from_value(
            json!({"type": "message", "id": id, "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": "answer", "annotations": []}]}),
        )
        .unwrap(),
    )
}

async fn item_count(pool: &DbPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn turn(store: &ResponseStore, id: &str, previous: Option<&str>) {
    store
        .persist(
            id,
            previous,
            vec![message(id), answer(&format!("msg_{id}"))],
            &ResponseMetadata::default(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn deleting_a_response_erases_its_prompt_and_answer() {
    let pool = pool().await;
    let store = ResponseStore::new(Arc::clone(&pool));
    turn(&store, "resp_only", None).await;
    assert_eq!(item_count(&pool).await, 2);

    store.delete("resp_only").await.unwrap();
    assert_eq!(item_count(&pool).await, 0);
}

#[tokio::test]
async fn rows_another_response_lists_survive_until_the_last_one_is_deleted() {
    let pool = pool().await;
    let store = ResponseStore::new(Arc::clone(&pool));
    // `resp_a` and `resp_b` are siblings: both inherit the parent's two rows.
    turn(&store, "resp_parent", None).await;
    turn(&store, "resp_a", Some("resp_parent")).await;
    turn(&store, "resp_b", Some("resp_parent")).await;
    assert_eq!(item_count(&pool).await, 6);

    // Children still list every parent row.
    store.delete("resp_parent").await.unwrap();
    assert_eq!(item_count(&pool).await, 6);
    // Only `resp_a`'s own rows go; `resp_b` still lists the inherited ones.
    store.delete("resp_a").await.unwrap();
    assert_eq!(item_count(&pool).await, 4);
    assert_eq!(store.rehydrate("resp_b").await.unwrap().len(), 4);
    store.delete("resp_b").await.unwrap();
    assert_eq!(item_count(&pool).await, 0);
}

#[tokio::test]
async fn deleting_a_conversation_latest_response_keeps_its_items_and_previous_state() {
    let pool = pool().await;
    let conversations = ConversationStore::new(Arc::clone(&pool));
    let responses = ResponseStore::new(Arc::clone(&pool));
    let conversation = conversations
        .create_with_metadata_and_items(Some("default_tenant"), None, vec![])
        .await
        .unwrap();
    let id = conversation.conversation_id.as_str();
    for (response, text) in [("resp_first", "first"), ("resp_second", "second")] {
        conversations
            .persist(
                id,
                response,
                None,
                vec![message(text), answer(&format!("msg_{response}"))],
                &ResponseMetadata::default(),
            )
            .await
            .unwrap();
    }
    let latest = |pool: Arc<DbPool>| async move {
        conversation_model::get(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .latest_response_id
    };
    assert_eq!(latest(Arc::clone(&pool)).await.as_deref(), Some("resp_second"));

    responses.delete("resp_second").await.unwrap();
    assert_eq!(
        latest(Arc::clone(&pool)).await.as_deref(),
        Some("resp_first"),
        "the conversation keeps the newest state it still has"
    );
    assert_eq!(
        item_count(&pool).await,
        4,
        "conversation items stay with the conversation"
    );

    responses.delete("resp_first").await.unwrap();
    assert_eq!(latest(Arc::clone(&pool)).await, None);
    assert_eq!(item_count(&pool).await, 4);
}
