//! CDP wire-format message types.
//!
//! CDP speaks one JSON object per WebSocket text frame:
//! - Request:  `{ id, method, params, sessionId? }`
//! - Response: `{ id, result?, error?: { code, message, data? } }`
//! - Event:    `{ method, params, sessionId? }`
//!
//! Responses carry `id` and no `method`; events carry `method` and no `id`.
//! We deserialize with an `#[serde(untagged)]` enum that tries `Response`
//! first, falling back to `Event`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An outgoing CDP request. `session_id` is `None` for browser-level commands.
#[derive(Debug, Serialize)]
pub struct CdpRequest {
    pub id: u32,
    pub method: String,
    #[serde(skip_serializing_if = "Value::is_null", default)]
    pub params: Value,
    #[serde(skip_serializing_if = "Option::is_none", rename = "sessionId")]
    pub session_id: Option<String>,
}

/// The flat error body returned on a failed CDP command.
#[derive(Debug, Deserialize)]
pub struct CdpErrorBody {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// A CDP response. Exactly one of `result`/`error` is present on success/failure.
#[derive(Debug, Deserialize)]
pub struct CdpResponse {
    pub id: u32,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<CdpErrorBody>,
}

/// A CDP event. `session_id` is `None` for browser-level events.
#[derive(Debug, Clone, Deserialize)]
pub struct CdpEvent {
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default, rename = "sessionId")]
    pub session_id: Option<String>,
}

/// Discriminated union of incoming messages.
///
/// Ordering matters: `Response` (has `id`, no `method`) is tried first;
/// `Event` (has `method`, no `id`) is the fallback.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Response(CdpResponse),
    Event(CdpEvent),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_omits_empty_params_and_missing_session() {
        let req = CdpRequest {
            id: 1,
            method: "Target.setAutoAttach".into(),
            params: serde_json::json!({"autoAttach": true}),
            session_id: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("sessionId"), "sessionId should be absent: {s}");
        assert!(s.contains("\"params\""));
    }

    #[test]
    fn request_serializes_session_id_when_present() {
        let req = CdpRequest {
            id: 2,
            method: "Page.navigate".into(),
            params: serde_json::json!({}),
            session_id: Some("ABC".into()),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"sessionId\":\"ABC\""), "{s}");
    }

    #[test]
    fn demux_response_with_result() {
        let v = serde_json::json!({ "id": 5, "result": { "ok": true } });
        let m: Message = serde_json::from_value(v).unwrap();
        match m {
            Message::Response(r) => {
                assert_eq!(r.id, 5);
                assert!(r.error.is_none());
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn demux_response_with_error() {
        let v = serde_json::json!({
            "id": 6,
            "error": { "code": -32000, "message": "boom", "data": "ctx" }
        });
        let m: Message = serde_json::from_value(v).unwrap();
        match m {
            Message::Response(r) => {
                let e = r.error.expect("error body");
                assert_eq!(e.code, -32000);
                assert_eq!(e.message, "boom");
                assert_eq!(e.data, Some(serde_json::json!("ctx")));
                assert!(r.result.is_none());
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn demux_event_no_session() {
        let v = serde_json::json!({ "method": "Target.attachedToTarget", "params": { "x": 1 } });
        let m: Message = serde_json::from_value(v).unwrap();
        match m {
            Message::Event(e) => {
                assert_eq!(e.method, "Target.attachedToTarget");
                assert!(e.session_id.is_none());
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn demux_event_with_session() {
        let v = serde_json::json!({
            "method": "Page.lifecycleEvent",
            "params": { "name": "load" },
            "sessionId": "S1"
        });
        let m: Message = serde_json::from_value(v).unwrap();
        match m {
            Message::Event(e) => {
                assert_eq!(e.method, "Page.lifecycleEvent");
                assert_eq!(e.session_id.as_deref(), Some("S1"));
            }
            _ => panic!("expected Event"),
        }
    }
}
