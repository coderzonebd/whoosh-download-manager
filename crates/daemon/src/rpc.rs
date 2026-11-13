//! JSON-RPC 2.0 framing and dispatch to a [`Service`].
//!
//! * Every WebSocket text frame holds one JSON value.
//! * Invalid JSON → `-32700`, id `null`.
//! * Anything that is not a request object → `-32600` (with the request's
//!   id when it has a usable one, `null` otherwise).
//! * **Batches are rejected**: a JSON array gets a single `-32600` error with
//!   id `null` and none of its members are executed. No client needs them,
//!   and they would complicate per-connection backpressure.
//! * Unknown method → `-32601`; params that do not fit the method → `-32602`.
//!   Params are by-name (an object); positional (array) params are `-32602`.
//! * A `ServiceError`'s code and message are passed through unchanged.
//! * Requests without an `id` are notifications: they are executed and never
//!   answered, not even with an error.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use whoosh_proto::method;
use whoosh_proto::service::{Service, ServiceError};

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// One parsed request (or client notification).
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// `None` for a notification. Otherwise a string, number or `null`.
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// What a frame turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request(Request),
    /// Not a valid request; answer with this (already built) error response.
    Invalid(Value),
    /// Not a valid request, and nothing should be sent back (an invalid
    /// message that is clearly a notification or a response).
    Ignore,
}

/// Parses one text frame.
pub fn parse(text: &str) -> Incoming {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            return Incoming::Invalid(error_response(Value::Null, PARSE_ERROR, "parse error"));
        }
    };
    let obj = match value {
        Value::Object(obj) => obj,
        Value::Array(_) => {
            return Incoming::Invalid(error_response(
                Value::Null,
                INVALID_REQUEST,
                "batch requests are not supported; send one request per message",
            ));
        }
        _ => {
            return Incoming::Invalid(error_response(
                Value::Null,
                INVALID_REQUEST,
                "a request must be a JSON object",
            ));
        }
    };
    parse_object(obj)
}

fn parse_object(mut obj: Map<String, Value>) -> Incoming {
    // A usable id is a string, a number or null. Anything else cannot be
    // echoed back, so the error goes out with id null.
    let id = match obj.remove("id") {
        None => None,
        Some(v @ (Value::String(_) | Value::Number(_) | Value::Null)) => Some(v),
        Some(_) => {
            return Incoming::Invalid(error_response(
                Value::Null,
                INVALID_REQUEST,
                "id must be a string, a number or null",
            ));
        }
    };
    let looks_like_response = obj.contains_key("result") || obj.contains_key("error");
    let invalid = |message: &str| match &id {
        Some(id) => Incoming::Invalid(error_response(id.clone(), INVALID_REQUEST, message)),
        None if looks_like_response => Incoming::Ignore,
        None => Incoming::Invalid(error_response(Value::Null, INVALID_REQUEST, message)),
    };
    if obj.get("jsonrpc") != Some(&Value::String("2.0".into())) {
        return invalid("jsonrpc must be \"2.0\"");
    }
    let method = match obj.remove("method") {
        Some(Value::String(m)) => m,
        Some(_) => return invalid("method must be a string"),
        None => return invalid("expected a request (no method)"),
    };
    // `"params": null` is read as "no params": not strictly JSON-RPC 2.0, but
    // common in clients, and harmless.
    let params = obj.remove("params").filter(|p| !p.is_null());
    if let Some(p) = &params
        && !(p.is_object() || p.is_array())
    {
        return invalid("params must be an object");
    }
    Incoming::Request(Request { id, method, params })
}

pub fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Runs a request against the service. Returns `Ok(result)` or the error.
/// `session.hello` is the session's business and never reaches here.
pub async fn dispatch(service: &Arc<dyn Service>, req: Request) -> Result<Value, ServiceError> {
    let params = req.params;
    match req.method.as_str() {
        method::DOWNLOAD_PROBE => to_value(service.probe(by_name(params)?).await?),
        method::DOWNLOAD_ADD => to_value(service.add(by_name(params)?).await?),
        method::DOWNLOAD_LIST => to_value(service.list(by_name(params)?).await?),
        method::DOWNLOAD_GET => to_value(service.get(by_name(params)?).await?),
        method::DOWNLOAD_PAUSE => to_value(service.pause(by_name(params)?).await?),
        method::DOWNLOAD_RESUME => to_value(service.resume(by_name(params)?).await?),
        method::DOWNLOAD_REMOVE => to_value(service.remove(by_name(params)?).await?),
        method::DOWNLOAD_REFRESH_URL => to_value(service.refresh_url(by_name(params)?).await?),
        method::NET_PRECONNECT => to_value(service.preconnect(by_name(params)?).await?),
        method::STATS_GET => {
            no_params(params)?;
            to_value(service.stats().await?)
        }
        method::SETTINGS_GET => {
            no_params(params)?;
            to_value(service.settings().await?)
        }
        method::SETTINGS_SET => to_value(service.set_settings(by_name(params)?).await?),
        other => Err(ServiceError::new(
            METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

/// Decodes by-name params. Absent params decode as `{}`, so methods whose
/// fields all have defaults (`download.list`) work without any.
fn by_name<T: DeserializeOwned>(params: Option<Value>) -> Result<T, ServiceError> {
    let value = match params {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(Value::Array(_)) => {
            return Err(ServiceError::invalid_params(
                "params must be an object (by-name), not an array",
            ));
        }
        Some(v) => v,
    };
    serde_json::from_value(value).map_err(|e| ServiceError::invalid_params(e.to_string()))
}

/// Methods without params accept none, `null`, `{}` or `[]`.
fn no_params(params: Option<Value>) -> Result<(), ServiceError> {
    match params {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(m)) if m.is_empty() => Ok(()),
        Some(Value::Array(a)) if a.is_empty() => Ok(()),
        Some(_) => Err(ServiceError::invalid_params("this method takes no params")),
    }
}

fn to_value<T: Serialize>(v: T) -> Result<Value, ServiceError> {
    serde_json::to_value(v).map_err(|e| ServiceError::new(INTERNAL_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid_code(text: &str) -> (i64, Value) {
        match parse(text) {
            Incoming::Invalid(v) => (v["error"]["code"].as_i64().unwrap(), v["id"].clone()),
            other => panic!("expected an error for {text}, got {other:?}"),
        }
    }

    #[test]
    fn classifies_frames() {
        assert_eq!(invalid_code("{"), (-32700, Value::Null));
        assert_eq!(invalid_code("[]"), (-32600, Value::Null));
        assert_eq!(
            invalid_code(r#"[{"jsonrpc":"2.0","id":1,"method":"stats.get"}]"#),
            (-32600, Value::Null)
        );
        assert_eq!(invalid_code("42"), (-32600, Value::Null));
        assert_eq!(
            invalid_code(r#"{"id":7,"method":"stats.get"}"#),
            (-32600, json!(7))
        );
        assert_eq!(
            invalid_code(r#"{"jsonrpc":"2.0","id":"a","method":3}"#),
            (-32600, json!("a"))
        );
        assert_eq!(
            invalid_code(r#"{"jsonrpc":"2.0","id":{"x":1},"method":"stats.get"}"#),
            (-32600, Value::Null)
        );
        assert_eq!(
            invalid_code(r#"{"jsonrpc":"2.0","id":1,"method":"m","params":5}"#),
            (-32600, json!(1))
        );
        // A stray response from the client is ignored, not answered.
        assert_eq!(parse(r#"{"jsonrpc":"2.0","result":1}"#), Incoming::Ignore);

        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","method":"download.pause","params":{"ids":[]}}"#),
            Incoming::Request(Request {
                id: None,
                method: "download.pause".into(),
                params: Some(json!({ "ids": [] })),
            })
        );
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","id":null,"method":"stats.get","params":null}"#),
            Incoming::Request(Request {
                id: Some(Value::Null),
                method: "stats.get".into(),
                params: None,
            })
        );
    }

    #[test]
    fn param_shapes() {
        assert!(no_params(None).is_ok());
        assert!(no_params(Some(json!({}))).is_ok());
        assert!(no_params(Some(json!([]))).is_ok());
        assert_eq!(no_params(Some(json!({"a":1}))).unwrap_err().code, -32602);
        let list: whoosh_proto::ListParams = by_name(None).unwrap();
        assert_eq!(list, whoosh_proto::ListParams::default());
        assert_eq!(
            by_name::<whoosh_proto::IdParams>(Some(json!(["dl_1"])))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(
            by_name::<whoosh_proto::IdParams>(Some(json!({})))
                .unwrap_err()
                .code,
            -32602
        );
    }
}
