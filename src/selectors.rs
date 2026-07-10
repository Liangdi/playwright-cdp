//! Selector-engine injection and element resolution.
//!
//! The engine JS ([`INJECTED_SCRIPT`]) is registered on `self.__pwcdpInjected`
//! and exposes `querySelectorAll(root, selector)`, `querySelector`, and
//! `elementState(el)`. Resolution here goes through `Runtime.callFunctionOn` and
//! returns CDP `RemoteObjectId`s; we resolve a single node at a time to avoid
//! materializing (and leaking) handles for every match.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use serde_json::{json, Value};

/// The injected selector-engine bundle, embedded at compile time.
pub const INJECTED_SCRIPT: &str = include_str!("../assets/injected_script.js");

/// Count elements matching `selector` in the given execution context.
pub async fn count(session: &CdpSession, ctx: Option<i64>, selector: &str) -> Result<usize> {
    let v = eval_context(
        session,
        ctx,
        "(sel) => self.__pwcdpInjected.querySelectorAll(document, sel).length",
        json!(selector),
    )
    .await?;
    v.as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| Error::ProtocolError(format!("selector count returned non-number: {v}")))
}

/// Resolve a single element (by index) to its `RemoteObjectId`, or `None` if
/// no element exists at that index.
pub async fn element_at(
    session: &CdpSession,
    ctx: Option<i64>,
    selector: &str,
    index: usize,
) -> Result<Option<String>> {
    eval_context_handle(
        session,
        ctx,
        "(arg) => { const e = self.__pwcdpInjected.querySelectorAll(document, arg.sel); return arg.index < e.length ? e[arg.index] : undefined; }",
        json!({ "sel": selector, "index": index }),
    )
    .await
}

/// Descend an ordered chain of same-origin iframes to reach the deepest
/// frame's `contentDocument`. `frame_chain` lists iframe selectors outermost
/// first; each is resolved in the previous frame's document (the first in the
/// top `document`). Returns the `RemoteObjectId` of the deepest content
/// document, or `None` if any hop is missing or cross-origin.
#[allow(dead_code)]
pub(crate) async fn frame_chain_document_handle(
    session: &CdpSession,
    ctx: Option<i64>,
    frame_chain: &[String],
) -> Result<Option<String>> {
    eval_context_handle(
        session,
        ctx,
        "(arg) => { let root = document; for (const fs of arg.chain) { const f = self.__pwcdpInjected.querySelectorAll(root, fs)[0]; root = f && f.contentDocument; if (!root) return undefined; } return root; }",
        json!({ "chain": frame_chain }),
    )
    .await
}

/// Like [`count`], but scoped to the `contentDocument` reached by descending
/// `frame_chain` (ordered same-origin iframe selectors, outermost first).
/// Returns `0` if any iframe in the chain is missing or cross-origin.
pub(crate) async fn count_in(
    session: &CdpSession,
    ctx: Option<i64>,
    frame_chain: &[String],
    selector: &str,
) -> Result<usize> {
    let v = eval_context(
        session,
        ctx,
        "(arg) => { let root = document; for (const fs of arg.chain) { const f = self.__pwcdpInjected.querySelectorAll(root, fs)[0]; root = f && f.contentDocument; if (!root) return 0; } return self.__pwcdpInjected.querySelectorAll(root, arg.sel).length; }",
        json!({ "chain": frame_chain, "sel": selector }),
    )
    .await?;
    v.as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| Error::ProtocolError(format!("selector count returned non-number: {v}")))
}

/// Like [`element_at`], but scoped to the `contentDocument` reached by
/// descending `frame_chain` (ordered same-origin iframe selectors, outermost
/// first). Returns `None` if the chain is unreachable or no element exists at
/// `index`.
pub(crate) async fn element_at_in(
    session: &CdpSession,
    ctx: Option<i64>,
    frame_chain: &[String],
    selector: &str,
    index: usize,
) -> Result<Option<String>> {
    eval_context_handle(
        session,
        ctx,
        "(arg) => { let root = document; for (const fs of arg.chain) { const f = self.__pwcdpInjected.querySelectorAll(root, fs)[0]; root = f && f.contentDocument; if (!root) return undefined; } const e = self.__pwcdpInjected.querySelectorAll(root, arg.sel); return arg.index < e.length ? e[arg.index] : undefined; }",
        json!({ "chain": frame_chain, "sel": selector, "index": index }),
    )
    .await
}

// --- low-level evaluation wrappers ------------------------------------------
//
// When a `ctx` (execution-context id) is supplied, evaluation runs in that
// context via `Runtime.evaluate { contextId }`. This is required for
// child-frame scoping: each frame has its own main-world context, and the
// top-level page's default context sees only its own document.
//
// When `ctx` is `None` we omit `contextId`, letting CDP pick the page's default
// main-world context. This is the legacy path and what most page-level helpers
// used before per-frame contexts were tracked.

/// Evaluate a `(arg) => ...` function in `ctx`'s execution context (or the
/// page's default context if `ctx` is `None`), returning by value.
pub(crate) async fn eval_context(
    session: &CdpSession,
    ctx: Option<i64>,
    function: &str,
    arg: Value,
) -> Result<Value> {
    let expr = format!("({})({})", function, serde_json::to_string(&arg)?);
    let mut params = json!({
        "expression": expr,
        "returnByValue": true,
        "awaitPromise": true,
    });
    if let Some(id) = ctx {
        params["contextId"] = json!(id);
    }
    let resp = session.send("Runtime.evaluate", params).await?;
    if let Some(exc) = resp.get("exceptionDetails") {
        let msg = exc
            .get("exception")
            .and_then(|e| e.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("evaluation threw");
        return Err(Error::ProtocolError(format!("eval error: {msg}")));
    }
    Ok(resp
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null))
}

/// Evaluate a `(arg) => ...` function in `ctx`'s execution context (or the
/// page's default context if `ctx` is `None`), returning a `RemoteObjectId` for
/// the returned object (or `None` for null/undefined).
pub(crate) async fn eval_context_handle(
    session: &CdpSession,
    ctx: Option<i64>,
    function: &str,
    arg: Value,
) -> Result<Option<String>> {
    let expr = format!("({})({})", function, serde_json::to_string(&arg)?);
    let mut params = json!({
        "expression": expr,
        "returnByValue": false,
        "awaitPromise": true,
    });
    if let Some(id) = ctx {
        params["contextId"] = json!(id);
    }
    let resp = session.send("Runtime.evaluate", params).await?;
    if let Some(exc) = resp.get("exceptionDetails") {
        let msg = exc
            .get("exception")
            .and_then(|e| e.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("evaluation threw");
        return Err(Error::ProtocolError(format!("eval error: {msg}")));
    }
    Ok(resp
        .get("result")
        .and_then(|r| r.get("objectId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// Evaluate an `(element, arg) => ...` function against an element handle, by value.
///
/// The element is passed as the first explicit argument (via its objectId) so
/// arrow functions work — `callFunctionOn` otherwise binds the element to
/// `this`, which arrow functions ignore.
pub(crate) async fn eval_object(
    session: &CdpSession,
    object_id: &str,
    function: &str,
    arg: Value,
) -> Result<Value> {
    let params = json!({
        "objectId": object_id,
        "functionDeclaration": function,
        "arguments": [{ "objectId": object_id }, { "value": arg }],
        "returnByValue": true,
        "awaitPromise": true,
    });
    let result = call(session, params).await?;
    Ok(result.get("value").cloned().unwrap_or(Value::Null))
}

/// Run `Runtime.callFunctionOn` and return the `result` object.
async fn call(session: &CdpSession, params: Value) -> Result<Value> {
    let resp = session.send("Runtime.callFunctionOn", params).await?;
    // CDP nests the remote object under "result".
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}
