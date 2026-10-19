use super::*;
use serde_json::json;

#[test]
fn item_resource_has_one_id_and_flattened_content() {
    let item = serde_json::from_value(json!({"type":"message", "id":"upstream_id", "role":"user", "content":"hello"}))
        .unwrap();
    let resource = ItemResponse::new("item_stored".into(), item);
    let text = serde_json::to_string(&resource).unwrap();
    assert_eq!(text.matches("\"id\"").count(), 1);
    let actual: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(actual["id"], "item_stored");
    assert_eq!(actual["type"], "message");
    assert_eq!(actual["status"], "completed");
    assert_eq!(actual["content"], json!([{"type":"input_text", "text":"hello"}]));
    assert!(actual.get("item").is_none());
}

#[test]
fn batch_requests_and_empty_pages_match_wire_contract() {
    let request: CreateItemRequest =
        serde_json::from_value(json!({"items":[{"role":"user", "content":"hello"}]})).unwrap();
    assert_eq!(request.items.len(), 1);
    assert!(serde_json::from_value::<CreateItemRequest>(json!({"item":{"role":"user", "content":"hello"}})).is_err());
    assert_eq!(
        serde_json::to_value(ListItemsResponse::new(vec![], false)).unwrap(),
        json!({"object":"list", "data":[], "has_more":false, "first_id":null, "last_id":null})
    );
}

#[test]
fn manually_added_assistant_shorthand_remains_input_text() {
    let item: InOutItem = serde_json::from_value(json!({"type":"message", "role":"assistant", "content":"9"})).unwrap();
    let resource = ItemResponse::new("msg_manual".into(), item);
    let actual = serde_json::to_value(resource).unwrap();
    assert_eq!(actual["role"], "assistant");
    assert_eq!(actual["content"], json!([{"type":"input_text", "text":"9"}]));
}
