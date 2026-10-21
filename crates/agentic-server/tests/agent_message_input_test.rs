//! Client-managed plaintext `agent_message` input is rejected before inference (#405).
//!
//! Hosted multi-agent orchestration publishes agent messages sealed as
//! `encrypted_content` and keeps them out of model context. A plaintext one comes from
//! client-managed collaboration, which the gateway does not implement; it used to be
//! dropped silently while the request succeeded.
#[allow(dead_code)]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Json, Router, routing::post};
use http::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Start a gateway whose upstream counts the inference requests it receives.
async fn gateway() -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let upstream = Router::new().route(
        "/v1/responses",
        post(move |Json(_): Json<Value>| {
            counter.fetch_add(1, Ordering::Relaxed);
            async {
                Json(json!({"id":"resp_fixture", "object":"response", "created_at":0,
                "model":"synthetic-model", "status":"completed", "output":[{
                    "id":"msg_fixture", "type":"message", "role":"assistant", "status":"completed",
                    "content":[{"type":"output_text", "text":"fixture", "annotations":[]}]}],
                "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = common::test_config(&format!("http://{}", listener.local_addr().unwrap()));
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let (url, _) = common::spawn_gateway(common::test_state(&config)).await;
    (url, calls)
}

/// The reproduction from #405: `store: false`, client namespace tools, no hosted `multi_agent`.
fn request(agent_message_content: &Value, stream: bool) -> Value {
    json!({
        "model": "synthetic-model",
        "store": false,
        "stream": stream,
        "input": [
            {"role": "user", "content": "Report the board update."},
            {"type": "agent_message", "author": "/root/worker_a", "recipient": "/root",
             "content": agent_message_content}
        ],
        "tools": [{"type": "namespace", "name": "collaboration", "description": "Client tools", "tools": [
            {"type": "function", "name": "wait_agent", "description": "Wait",
             "parameters": {"type": "object", "properties": {}}}
        ]}]
    })
}

#[tokio::test]
async fn plaintext_agent_messages_fail_before_inference_and_sealed_ones_are_replayed() {
    let (url, upstream_calls) = gateway().await;
    let client = reqwest::Client::new();
    let plaintext = json!([{"type": "input_text",
        "text": "Message Type: CHANNEL_POST\nSender: /root/worker_a\nChannel: coordination\nPayload:\nSYNTHETIC_BOARD_MARKER"}]);

    for stream in [false, true] {
        let response = client
            .post(format!("{url}/v1/responses"))
            .json(&request(&plaintext, stream))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "stream: {stream}");
        let body: Value = response.json().await.unwrap();
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.starts_with(
                "invalid request: input[1].content[0]: agent_message content part type `input_text` is not supported"
            ),
            "{body}"
        );
    }
    assert_eq!(upstream_calls.load(Ordering::Relaxed), 0, "nothing reaches inference");

    // A sealed transcript item from hosted orchestration is still accepted for replay.
    let sealed = json!([{"type": "encrypted_content", "encrypted_content": "opaque"}]);
    let response = client
        .post(format!("{url}/v1/responses"))
        .json(&request(&sealed, false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(upstream_calls.load(Ordering::Relaxed), 1);
}
