//! `GET /v1/responses/{id}/input_items` lists the stored context a response was generated from.
#[allow(dead_code)]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agentic_core::executor::ExecutionContext;
use agentic_core::storage::{ResponseMetadata, ResponseStore};
use axum::{Json, Router, routing::post};
use http::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Mock inference upstream answering every turn with one uniquely identified message.
fn answering_upstream() -> Router {
    let next_message_id = Arc::new(AtomicUsize::new(0));
    Router::new().route(
        "/v1/responses",
        post(move |Json(_): Json<Value>| {
            let message_id = next_message_id.fetch_add(1, Ordering::Relaxed);
            async move {
                Json(json!({"id":"resp_upstream", "object":"response", "created_at":123,
                "model":"test-model", "status":"completed", "output":[{
                    "type":"message", "id":format!("msg_answer_{message_id}"), "role":"assistant",
                    "status":"completed", "content":[{"type":"output_text", "text":format!("answer {message_id}")}]}],
                "usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}}))
            }
        }),
    )
}

struct Harness {
    url: String,
    client: reqwest::Client,
    exec: Arc<ExecutionContext>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Harness {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = common::test_config(&format!("http://{}", listener.local_addr().unwrap()));
        let upstream = tokio::spawn(async move { axum::serve(listener, answering_upstream()).await.unwrap() });
        config.db_url = Some("sqlite://?mode=memory".into());
        let exec = Arc::new(ExecutionContext::from_config(&config).await.unwrap());
        let mut state = common::test_state(&config);
        state.exec_ctx = Arc::clone(&exec);
        let (url, gateway) = common::spawn_gateway(state).await;
        Self {
            url,
            client: reqwest::Client::new(),
            exec,
            tasks: vec![upstream, gateway],
        }
    }

    async fn create(&self, body: Value) -> Value {
        let response = self
            .client
            .post(format!("{}/v1/responses", self.url))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    async fn input_items(&self, id: &str, query: &str) -> reqwest::Response {
        self.client
            .get(format!("{}/v1/responses/{id}/input_items{query}", self.url))
            .bearer_auth("test-key")
            .send()
            .await
            .unwrap()
    }

    async fn page(&self, id: &str, query: &str) -> Value {
        let response = self.input_items(id, query).await;
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    async fn stop(self) {
        for task in self.tasks {
            task.abort();
            let _ = task.await;
        }
        self.exec.storage_pool().unwrap().close().await;
    }
}

/// The text of each item on a page: input text, or the first content part's text.
fn texts(page: &Value) -> Vec<String> {
    page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            item["content"]
                .as_str()
                .or_else(|| item["content"][0]["text"].as_str())
                .expect("text item")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn input_items_list_the_inherited_context_without_the_response_own_output() {
    let harness = Harness::start().await;
    let first = harness
        .create(json!({"model":"test-model", "input":"first", "store":true}))
        .await;
    let first_id = first["id"].as_str().unwrap();
    let second = harness
        .create(json!({"model":"test-model", "input":"second", "store":true, "previous_response_id":first_id}))
        .await;
    let second_id = second["id"].as_str().unwrap();

    // The first turn's own answer is input to the second, but neither lists its own output.
    assert_eq!(texts(&harness.page(first_id, "?order=asc").await), ["first"]);
    let ascending = harness.page(second_id, "?order=asc").await;
    assert_eq!(texts(&ascending), ["first", "answer 0", "second"]);
    assert_eq!(ascending["object"], "list");
    assert_eq!(ascending["has_more"], false);
    assert_eq!(ascending["data"][1]["id"], "msg_answer_0");
    assert!(
        ascending["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != "msg_answer_1"),
        "a response's own output is not one of its inputs"
    );
    // Like conversation item listing, the default order is newest first.
    assert_eq!(
        texts(&harness.page(second_id, "").await),
        ["second", "answer 0", "first"]
    );

    // Pagination walks the same sequence one item at a time.
    let mut cursor = String::new();
    let mut walked = Vec::new();
    loop {
        let page = harness.page(second_id, &format!("?order=asc&limit=1{cursor}")).await;
        walked.extend(texts(&page));
        assert_eq!(page["first_id"], page["last_id"]);
        if page["has_more"] == false {
            break;
        }
        cursor = format!("&after={}", page["last_id"].as_str().unwrap());
    }
    assert_eq!(walked, ["first", "answer 0", "second"]);

    harness.stop().await;
}

#[tokio::test]
async fn input_items_of_a_conversation_turn_reflect_the_conversation_at_that_turn() {
    let harness = Harness::start().await;
    let conversation: Value = harness
        .client
        .post(format!("{}/v1/conversations", harness.url))
        .bearer_auth("test-key")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conversation_id = conversation["id"].as_str().unwrap();
    let first = harness
        .create(json!({"model":"test-model", "input":"first", "conversation":conversation_id}))
        .await;
    let second = harness
        .create(json!({"model":"test-model", "input":"second", "conversation":conversation_id}))
        .await;
    // A later turn does not leak into an earlier response's inputs.
    harness
        .create(json!({"model":"test-model", "input":"third", "conversation":conversation_id}))
        .await;

    assert_eq!(
        texts(&harness.page(first["id"].as_str().unwrap(), "?order=asc").await),
        ["first"]
    );
    assert_eq!(
        texts(&harness.page(second["id"].as_str().unwrap(), "?order=asc").await),
        ["first", "answer 0", "second"]
    );
    harness.stop().await;
}

#[tokio::test]
async fn input_items_reject_what_retrieval_rejects() {
    let harness = Harness::start().await;
    let stored = harness
        .create(json!({"model":"test-model", "input":"first", "store":true}))
        .await;
    let stored_id = stored["id"].as_str().unwrap();
    let transient = harness
        .create(json!({"model":"test-model", "input":"transient", "store":false}))
        .await;

    let unauthenticated = harness
        .client
        .get(format!("{}/v1/responses/{stored_id}/input_items", harness.url))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    for missing in [transient["id"].as_str().unwrap(), "resp_missing"] {
        assert_eq!(harness.input_items(missing, "").await.status(), StatusCode::NOT_FOUND);
    }
    for query in [
        "?limit=0",
        "?limit=101",
        "?order=sideways",
        "?include=reasoning.encrypted_content",
    ] {
        assert_eq!(
            harness.input_items(stored_id, query).await.status(),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    // Same contract as conversation item listing: an unknown cursor is a not-found naming `after`.
    let unknown_cursor = harness.input_items(stored_id, "?after=msg_unknown").await;
    assert_eq!(unknown_cursor.status(), StatusCode::NOT_FOUND);
    assert_eq!(unknown_cursor.json::<Value>().await.unwrap()["error"]["param"], "after");

    // A record without a response snapshot cannot tell its output from its input.
    let store = ResponseStore::new(Arc::new(harness.exec.storage_pool().unwrap().clone()));
    store
        .persist("resp_legacy", None, Vec::new(), &ResponseMetadata::default())
        .await
        .unwrap();
    assert_eq!(
        harness.input_items("resp_legacy", "").await.status(),
        StatusCode::CONFLICT
    );

    // Deleting a response removes its input listing with it.
    let deleted = harness
        .client
        .delete(format!("{}/v1/responses/{stored_id}", harness.url))
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(harness.input_items(stored_id, "").await.status(), StatusCode::NOT_FOUND);
    harness.stop().await;
}
