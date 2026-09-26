pub mod account;
pub mod health;
pub mod rpc;
pub mod sponsor;
pub mod submit;

use alloy::primitives::Address;
use axum::{http::StatusCode, Json};
use serde_json::{json, Value};

pub(super) fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.into() })))
}

pub(super) fn internal_error(msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": msg.into() })))
}

pub(super) fn parse_owner(s: &str) -> Result<Address, (StatusCode, Json<Value>)> {
    s.parse::<Address>().map_err(|_| bad_request("Invalid owner address"))
}
