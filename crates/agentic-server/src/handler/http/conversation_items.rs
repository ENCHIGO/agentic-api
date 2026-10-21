//! HTTP transport for conversation item CRUD.

use std::borrow::Cow;

use axum::extract::{Path, RawQuery, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[cfg(feature = "openapi")]
use agentic_core::types::ConversationResponse;
use agentic_core::types::conversations::{ItemInclude, ItemLimit, ItemOrder, ItemPage};
use agentic_core::types::{CreateItemRequest, ItemResponse, ListItemsResponse};

use super::super::common::{error_response, executor_error_response, extract_json, read_bytes};
use super::conversations::{DEFAULT_TENANT_ID, conversation_response};
use crate::app::AppState;

/// Query parameters for item listings.
///
/// Parsed from the raw query string by [`ListItemsQuery::parse`]: `include` may
/// repeat, which form deserialization into a struct does not support.
#[derive(Debug, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ListItemsQuery {
    /// Maximum number of items to return (default: 20, max: 100).
    #[cfg_attr(feature = "openapi", param(value_type = Option<i64>, minimum = 1, maximum = 100))]
    pub limit: ItemLimit,

    /// Cursor for pagination (item ID to start after).
    pub after: Option<String>,

    /// Sort order for items (default: desc). Ascending order is oldest-first.
    #[cfg_attr(feature = "openapi", param(value_type = Option<ItemOrder>))]
    pub order: ItemOrder,

    /// Extra item fields to include, repeatable and also accepted as `include[]`.
    #[cfg_attr(feature = "openapi", param(value_type = Option<Vec<ItemInclude>>))]
    pub include: Vec<ItemInclude>,
}

impl ListItemsQuery {
    /// Parse and validate the parameters shared by every item listing.
    ///
    /// Unknown parameters are ignored, as typed query extraction does.
    ///
    /// # Errors
    /// Returns a 400 response naming the offending parameter for a repeated
    /// single-valued parameter, an out-of-range limit, an unknown order, or an
    /// include value that is unknown or that stored items cannot serve.
    pub(super) fn parse(raw: Option<&str>) -> Result<Self, Box<Response>> {
        let invalid = |message: String| Box::new(error_response(StatusCode::BAD_REQUEST, "invalid_request", &message));
        let mut query = Self::default();
        let (mut limit, mut after, mut order) = (None, None, None);
        for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
            let slot = match key.as_ref() {
                "limit" => &mut limit,
                "after" => &mut after,
                "order" => &mut order,
                "include" | "include[]" => {
                    let include = value
                        .parse::<ItemInclude>()
                        .map_err(|_| invalid(format!("unknown include value '{value}'")))?;
                    if !include.is_stored_with_items() {
                        return Err(invalid(format!("include value '{value}' is not supported")));
                    }
                    query.include.push(include);
                    continue;
                }
                _ => continue,
            };
            if slot.replace(value.clone()).is_some() {
                return Err(invalid(format!("'{key}' must be given at most once")));
            }
        }
        if let Some(limit) = limit {
            query.limit = limit
                .parse::<i64>()
                .ok()
                .and_then(|limit| ItemLimit::try_from(limit).ok())
                .ok_or_else(|| invalid("limit must be between 1 and 100".to_owned()))?;
        }
        if let Some(order) = order {
            query.order = match order.as_ref() {
                "asc" => ItemOrder::Asc,
                "desc" => ItemOrder::Desc,
                _ => return Err(invalid("order must be 'asc' or 'desc'".to_owned())),
            };
        }
        query.after = after.map(Cow::into_owned);
        Ok(query)
    }

    /// The validated pagination, borrowing the cursor.
    pub(super) fn page(&self) -> ItemPage<'_> {
        ItemPage {
            limit: self.limit,
            after: self.after.as_deref(),
            order: self.order,
        }
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/v1/conversations/{conversation_id}/items",
    params(
        ("conversation_id" = String, Path, description = "Conversation ID")
    ),
    request_body = CreateItemRequest,
    responses(
        (status = 200, description = "Items created", body = ListItemsResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Conversation not found"),
    ),
    security(("bearer_auth" = [])),
    tag = "conversations",
))]
pub async fn create_item(State(state): State<AppState>, Path(conversation_id): Path<String>, req: Request) -> Response {
    let (_, body) = req.into_parts();
    let bytes = match read_bytes(body, state.max_request_body_size).await {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };

    let request: CreateItemRequest = match extract_json(&bytes) {
        Ok(request) => request,
        Err(error) => return error,
    };

    match state
        .exec_ctx
        .conv_handler
        .create_items(DEFAULT_TENANT_ID, &conversation_id, request.items)
        .await
    {
        Ok(item_responses) => {
            // Return list response (matches OpenAI format)
            let response = agentic_core::types::ListItemsResponse::new(item_responses, false);
            axum::Json(response).into_response()
        }
        Err(e) => executor_error_response(e),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/v1/conversations/{conversation_id}/items",
    params(
        ("conversation_id" = String, Path, description = "Conversation ID"),
        ListItemsQuery
    ),
    responses(
        (status = 200, description = "Items retrieved", body = ListItemsResponse),
        (status = 404, description = "Conversation not found"),
    ),
    security(("bearer_auth" = [])),
    tag = "conversations",
))]
pub async fn list_items(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Response {
    let query = match ListItemsQuery::parse(raw.as_deref()) {
        Ok(query) => query,
        Err(error) => return *error,
    };

    match state
        .exec_ctx
        .conv_handler
        .list_items(DEFAULT_TENANT_ID, &conversation_id, query.page())
        .await
    {
        Ok(response) => axum::Json(response).into_response(),
        Err(e) => executor_error_response(e),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/v1/conversations/{conversation_id}/items/{item_id}",
    params(
        ("conversation_id" = String, Path, description = "Conversation ID"),
        ("item_id" = String, Path, description = "Item ID")
    ),
    responses(
        (status = 200, description = "Item retrieved", body = ItemResponse),
        (status = 404, description = "Item or conversation not found"),
    ),
    security(("bearer_auth" = [])),
    tag = "conversations",
))]
pub async fn retrieve_item(
    State(state): State<AppState>,
    Path((conversation_id, item_id)): Path<(String, String)>,
) -> Response {
    match state
        .exec_ctx
        .conv_handler
        .retrieve_item(DEFAULT_TENANT_ID, &conversation_id, &item_id)
        .await
    {
        Ok(response) => axum::Json(response).into_response(),
        Err(e) => executor_error_response(e),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    delete,
    path = "/v1/conversations/{conversation_id}/items/{item_id}",
    params(
        ("conversation_id" = String, Path, description = "Conversation ID"),
        ("item_id" = String, Path, description = "Item ID")
    ),
    responses(
        (status = 200, description = "Item deleted", body = ConversationResponse),
        (status = 404, description = "Item or conversation not found"),
    ),
    security(("bearer_auth" = [])),
    tag = "conversations",
))]
pub async fn delete_item(
    State(state): State<AppState>,
    Path((conversation_id, item_id)): Path<(String, String)>,
) -> Response {
    match state
        .exec_ctx
        .conv_handler
        .delete_item(DEFAULT_TENANT_ID, &conversation_id, &item_id)
        .await
    {
        Ok(conversation) => conversation_response(conversation),
        Err(e) => executor_error_response(e),
    }
}
