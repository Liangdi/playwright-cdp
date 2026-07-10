//! `JSHandle` — a reference to a remote JavaScript object (a CDP
//! `RemoteObjectId`) on a session.
//!
//! This is the Playwright analogue of `JSHandle`: a thin pointer into a remote
//! execution context. [`ElementHandle`](crate::element_handle::ElementHandle) is
//! a specialization of this for DOM nodes; here we model arbitrary objects.
//!
//! Unlike `ElementHandle` (which holds a full `Page`), `JSHandle` keeps only the
//! [`CdpSession`] it belongs to plus the object's `objectId`. Remote objects are
//! garbage-collected by the browser when their execution context is destroyed
//! (e.g. on navigation); call [`JSHandle::dispose`] to release them eagerly.
//!
//! All operations speak CDP directly:
//! - [`JSHandle::json_value`] / [`JSHandle::evaluate`] use `Runtime.callFunctionOn`
//!   with the object bound as the first explicit argument.
//! - [`JSHandle::get_property`] / [`JSHandle::get_properties`] use
//!   `Runtime.getProperties` (and `callFunctionOn` for a single named property).
//! - [`JSHandle::dispose`] uses `Runtime.releaseObject`.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// A reference to a remote JavaScript object.
///
/// Cheaply cloneable; all clones share the same session and `objectId`.
#[derive(Clone)]
pub struct JSHandle {
    /// The CDP session the remote object lives on (page- or worker-level).
    session: Arc<CdpSession>,
    /// The remote object id (`Runtime.RemoteObjectId`).
    object_id: String,
    /// Optional execution context the object was materialized in. Retained for
    /// provenance only; we route calls through `objectId` rather than a context
    /// id so that a stale (post-navigation) context doesn't poison lookups.
    context_id: Option<i64>,
}

impl JSHandle {
    pub(crate) fn new(session: Arc<CdpSession>, object_id: String) -> Self {
        Self {
            session,
            object_id,
            context_id: None,
        }
    }

    /// Like [`new`](Self::new), but records the execution context the object was
    /// created in. The context id is informational; evaluation always targets
    /// the `objectId` directly.
    pub(crate) fn with_context(
        session: Arc<CdpSession>,
        object_id: String,
        context_id: Option<i64>,
    ) -> Self {
        Self {
            session,
            object_id,
            context_id,
        }
    }

    /// The underlying CDP remote object id.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }

    /// The execution context this object was created in, if known.
    pub fn context_id(&self) -> Option<i64> {
        self.context_id
    }

    /// The owning CDP session.
    pub fn session(&self) -> &Arc<CdpSession> {
        &self.session
    }

    /// Return a JSON representation of the remote object (by value).
    ///
    /// Evaluates `e => e` against the object with `returnByValue`, so the
    /// browser serializes it (including nested structures) into JSON. Functions
    /// and non-JSON-serializable values become `{}` or are dropped by the
    /// browser's serializer, matching Playwright's behaviour.
    pub async fn json_value(&self) -> Result<Value> {
        let v = self
            .call_function_on_returning_value("(e) => e", Value::Null)
            .await?;
        Ok(v)
    }

    /// Fetch a single named property as a new [`JSHandle`].
    ///
    /// Uses `Runtime.getProperties` on the object (own + prototype chain) and
    /// wraps the matching property value's `objectId`. Returns an error if the
    /// property exists but is a primitive (no `objectId`) — use
    /// [`get_property_value`](Self::get_property_value) for primitives.
    pub async fn get_property(&self, name: &str) -> Result<JSHandle> {
        let resp = self
            .session
            .send(
                "Runtime.getProperties",
                json!({
                    "objectId": self.object_id,
                    "ownProperties": false,
                    "accessorPropertiesOnly": false,
                    "generatePreview": false,
                }),
            )
            .await?;

        let result = resp
            .get("result")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::ProtocolError("Runtime.getProperties missing result".into()))?;

        for prop in result {
            let matches = prop
                .get("name")
                .and_then(|v| v.as_str())
                .map(|n| n == name)
                .unwrap_or(false);
            if matches {
                if let Some(value) = prop.get("value") {
                    if let Some(oid) = value.get("objectId").and_then(|v| v.as_str()) {
                        return Ok(JSHandle::new(Arc::clone(&self.session), oid.to_string()));
                    }
                    // Property exists but is a primitive with no objectId.
                    return Err(Error::ProtocolError(format!(
                        "property '{name}' is not a remote object (no objectId)"
                    )));
                }
            }
        }
        Err(Error::ProtocolError(format!(
            "property '{name}' not found on remote object"
        )))
    }

    /// Fetch a single named property's primitive value (by value), or `None`
    /// if it is absent or not present as a JSON value.
    pub async fn get_property_value(&self, name: &str) -> Result<Option<Value>> {
        let function = format!("(e) => e === null || e === undefined ? null : e[{name_js}]", name_js = serde_json::to_string(name)?);
        let v = self
            .call_function_on_returning_value(&function, Value::Null)
            .await?;
        if v.is_null() {
            Ok(None)
        } else {
            Ok(Some(v))
        }
    }

    /// Fetch all own properties as a map of name → [`JSHandle`].
    ///
    /// Calls `Runtime.getProperties` with `ownProperties: true` and wraps each
    /// property value that carries an `objectId`. Properties whose values are
    /// primitives (no `objectId`) are wrapped via a `callFunctionOn` round-trip
    /// that boxes them into remote objects, so the map is complete.
    pub async fn get_properties(&self) -> Result<HashMap<String, JSHandle>> {
        let resp = self
            .session
            .send(
                "Runtime.getProperties",
                json!({
                    "objectId": self.object_id,
                    "ownProperties": true,
                    "accessorPropertiesOnly": false,
                    "generatePreview": false,
                }),
            )
            .await?;

        let result = resp
            .get("result")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::ProtocolError("Runtime.getProperties missing result".into()))?;

        let mut out = HashMap::new();
        for prop in result {
            let name = match prop.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Prefer an existing objectId (object-typed properties).
            if let Some(value) = prop.get("value") {
                if let Some(oid) = value.get("objectId").and_then(|v| v.as_str()) {
                    out.insert(name, JSHandle::new(Arc::clone(&self.session), oid.to_string()));
                    continue;
                }
            }
            // Otherwise box the primitive value into a remote object via
            // callFunctionOn so the caller still gets a JSHandle for it.
            if let Some(handle) = self.box_property(&name).await? {
                out.insert(name, handle);
            }
        }
        Ok(out)
    }

    /// Evaluate `pageFunction` against this object, returning a typed result by
    /// value.
    ///
    /// `pageFunction` is wrapped as `(e, arg) => { return (<pageFunction>); }`
    /// and run with this object bound as the first explicit argument (its
    /// `objectId`) so that arrow functions work under `callFunctionOn`. The
    /// result is returned by value and deserialized into `R`.
    pub async fn evaluate<R: DeserializeOwned>(
        &self,
        page_function: &str,
        arg: Option<Value>,
    ) -> Result<R> {
        let function = format!("(e, arg) => {{ return ({page_function}); }}");
        let v = self
            .call_function_on_returning_value(&function, arg.unwrap_or(Value::Null))
            .await?;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate `pageFunction` against this object, returning a new
    /// [`JSHandle`] to the resulting remote object (returned by reference, not
    /// by value). Useful when the function returns an object or DOM node you
    /// want to keep poking at.
    pub async fn evaluate_handle(
        &self,
        page_function: &str,
        arg: Option<Value>,
    ) -> Result<JSHandle> {
        let function = format!("(e, arg) => {{ return ({page_function}); }}");
        let params = json!({
            "objectId": self.object_id,
            "functionDeclaration": function,
            "arguments": [{ "value": arg.unwrap_or(Value::Null) }],
            "returnByValue": false,
            "awaitPromise": true,
        });
        let resp = self.session.send("Runtime.callFunctionOn", params).await?;
        if let Some(exc) = resp.get("exceptionDetails") {
            let msg = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("evaluation threw");
            return Err(Error::ProtocolError(format!("eval error: {msg}")));
        }
        let oid = resp
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::ProtocolError(
                    "callFunctionOn did not return a remote object (no objectId)".into(),
                )
            })?
            .to_string();
        Ok(JSHandle::new(Arc::clone(&self.session), oid))
    }

    /// Release the remote object eagerly (otherwise the browser GCs it on
    /// navigation). Idempotent: errors (e.g. already-released) are swallowed.
    pub async fn dispose(&self) -> Result<()> {
        let _ = self
            .session
            .send(
                "Runtime.releaseObject",
                json!({ "objectId": self.object_id }),
            )
            .await;
        Ok(())
    }

    // --- internal helpers ---------------------------------------------------

    /// Run `Runtime.callFunctionOn` with this object bound as the first
    /// argument (`this`/`e`), returning the result by value.
    async fn call_function_on_returning_value(
        &self,
        function: &str,
        arg: Value,
    ) -> Result<Value> {
        let params = json!({
            "objectId": self.object_id,
            "functionDeclaration": function,
            // Pass the object as an explicit first argument so arrow functions
            // (which ignore `this`) still receive it.
            "arguments": [{ "objectId": self.object_id }, { "value": arg }],
            "returnByValue": true,
            "awaitPromise": true,
        });
        let resp = self.session.send("Runtime.callFunctionOn", params).await?;
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

    /// Box a single named property's value into a remote object, returning a
    /// fresh [`JSHandle`] for it (or `None` if the property is absent). Used by
    /// [`get_properties`](Self::get_properties) for primitive-valued props.
    async fn box_property(&self, name: &str) -> Result<Option<JSHandle>> {
        let function = format!(
            "(e) => {{ const v = e == null ? undefined : e[{name_js}]; return v === undefined ? null : {{ boxed: v }}; }}",
            name_js = serde_json::to_string(name)?
        );
        let params = json!({
            "objectId": self.object_id,
            "functionDeclaration": function,
            // Pass the object as an explicit first argument so arrow functions
            // (which ignore `this`) still receive it.
            "arguments": [{ "objectId": self.object_id }],
            "returnByValue": false,
            "awaitPromise": true,
        });
        let resp = self.session.send("Runtime.callFunctionOn", params).await?;
        if resp.get("exceptionDetails").is_some() {
            return Ok(None);
        }
        let oid = match resp
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(|v| v.as_str())
        {
            Some(o) => o.to_string(),
            None => return Ok(None),
        };
        // De-reference `.boxed` so the returned handle points at the value
        // itself rather than the wrapper object.
        let inner = JSHandle::new(Arc::clone(&self.session), oid);
        match inner.get_property("boxed").await {
            Ok(h) => Ok(Some(h)),
            Err(_) => Ok(Some(inner)),
        }
    }
}
