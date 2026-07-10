//! `Accessibility` — accessibility-tree snapshot via the CDP `Accessibility`
//! domain.
//!
//! [`Page::accessibility`](crate::Page::accessibility) returns an
//! [`Accessibility`] handle bound to the page-level CDP session.
//! [`Accessibility::snapshot`] captures a point-in-time view of the page's
//! accessibility tree and returns it as an [`AccessibilityNode`] tree.
//!
//! CDP exposes two relevant commands:
//! - `Accessibility.getFullAXTree` — the entire page's accessibility tree
//!   (used when no `root` is requested).
//! - `Accessibility.getPartialAXTree` — the subtree rooted at a given
//!   `backendNodeId` (used when `root` is supplied).
//!
//! Both return a flat `nodes` array. We rebuild the parent/child relationships
//! from each node's `childIds` into the nested [`AccessibilityNode`] shape that
//! mirrors Playwright's `Accessibility.snapshot`.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// An accessibility snapshot handle for a [`Page`](crate::Page).
///
/// Cheaply cloneable; all clones share the same underlying page-level session.
#[derive(Clone)]
pub struct Accessibility {
    inner: Arc<AccessibilityInner>,
}

struct AccessibilityInner {
    /// The page-level CDP session (Accessibility domain).
    session: CdpSession,
}

impl Accessibility {
    pub(crate) fn new(session: CdpSession) -> Self {
        Self {
            inner: Arc::new(AccessibilityInner { session }),
        }
    }

    /// Capture an accessibility-tree snapshot.
    ///
    /// Without `options` (or with `interesting_only: true`), this fetches the
    /// full accessibility tree and prunes uninteresting nodes (those without a
    /// role/name/value/children), mirroring Playwright's default. When `root`
    /// is supplied, the partial tree rooted at that node is fetched instead via
    /// `Accessibility.getPartialAXTree`.
    pub async fn snapshot(
        &self,
        options: Option<AccessibilitySnapshotOptions>,
    ) -> Result<Option<AccessibilityNode>> {
        let opts = options.unwrap_or_default();
        let interesting_only = opts.interesting_only.unwrap_or(true);

        // Best-effort enable; some Chrome builds require it before the get*
        // commands return data.
        let _ = self.inner.session.send("Accessibility.enable", json!({})).await;

        let nodes = if let Some(root_node_id) = &opts.root {
            self.partial_tree(root_node_id).await?
        } else {
            let resp: Value = self
                .inner
                .session
                .send("Accessibility.getFullAXTree", json!({}))
                .await?;
            resp.get("nodes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        };

        if nodes.is_empty() {
            return Ok(None);
        }

        let root_id = match nodes.first().and_then(|n| n.get("nodeId")).and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(None),
        };

        let tree = build_tree(&nodes, &root_id, interesting_only)?;
        Ok(tree)
    }

    /// Fetch the partial accessibility tree rooted at `backend_node_id` via
    /// `Accessibility.getPartialAXTree`.
    async fn partial_tree(&self, backend_node_id: &str) -> Result<Vec<Value>> {
        // `backendNodeId` is an integer in CDP; accept a numeric string.
        let backend: i64 = backend_node_id
            .parse()
            .map_err(|_| Error::InvalidArgument("root must be a numeric backendNodeId".into()))?;
        let resp: Value = self
            .inner
            .session
            .send(
                "Accessibility.getPartialAXTree",
                json!({ "backendNodeId": backend }),
            )
            .await?;
        Ok(resp
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }
}

/// Options for [`Accessibility::snapshot`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AccessibilitySnapshotOptions {
    /// When `true` (the default), uninteresting nodes (no role/name/value and
    /// no interesting children) are pruned from the result. When `false`, the
    /// raw tree is returned verbatim.
    pub interesting_only: Option<bool>,
    /// A backend node id to snapshot the partial subtree of, instead of the
    /// whole page. Maps to `Accessibility.getPartialAXTree.backendNodeId`.
    pub root: Option<String>,
}

impl AccessibilitySnapshotOptions {
    pub fn interesting_only(mut self, v: bool) -> Self {
        self.interesting_only = Some(v);
        self
    }
    pub fn root(mut self, v: impl Into<String>) -> Self {
        self.root = Some(v.into());
        self
    }
}

/// A single node in an accessibility-tree snapshot.
///
/// Mirrors the useful subset of Playwright's `AccessibilityNode` and CDP's
/// `AXNode`. Fields are optional since CDP omits any that don't apply.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AccessibilityNode {
    /// The ARIA/DOM role (e.g. `"button"`, `"textbox"`).
    pub role: Option<String>,
    /// The accessible name.
    pub name: Option<String>,
    /// The current value, when the node has one.
    pub value: Option<Value>,
    /// The accessible description.
    pub description: Option<String>,
    /// Checked state: `"checked"`, `"unchecked"`, `"mixed"`, or `None`.
    pub checked: Option<String>,
    /// Whether the node is disabled.
    pub disabled: Option<bool>,
    /// Expanded state: `"expanded"`, `"collapsed"`, or `None`.
    pub expanded: Option<String>,
    /// Whether the node is focused.
    pub focused: Option<bool>,
    /// Whether the node is modal.
    pub modal: Option<bool>,
    /// Multi-line state for text inputs.
    pub multiline: Option<bool>,
    /// Read-only state.
    pub readonly: Option<bool>,
    /// Required state.
    pub required: Option<bool>,
    /// Selected state.
    pub selected: Option<bool>,
    /// Whether the node is hidden from the accessibility tree.
    pub hidden: Option<bool>,
    /// Child nodes.
    pub children: Vec<AccessibilityNode>,
}

impl AccessibilityNode {
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// Rebuild a nested [`AccessibilityNode`] tree from CDP's flat `nodes` array.
///
/// `interesting_only` controls pruning: a node is kept if it has any
/// meaningful property (role/name/value/description or a non-empty children
/// list after pruning), and dropped otherwise — matching Playwright's default.
fn build_tree(
    nodes: &[Value],
    root_id: &str,
    interesting_only: bool,
) -> Result<Option<AccessibilityNode>> {
    // Index nodes by their `nodeId` for O(1) lookup.
    let mut by_id: HashMap<String, &Value> = HashMap::with_capacity(nodes.len());
    for n in nodes {
        if let Some(id) = n.get("nodeId").and_then(|v| v.as_str()) {
            by_id.insert(id.to_string(), n);
        }
    }
    let root = by_id.get(root_id).ok_or_else(|| {
        Error::ProtocolError(format!(
            "accessibility tree root node '{root_id}' not found in response"
        ))
    })?;
    let mut node = convert_node(root, &by_id, interesting_only)?;
    if interesting_only {
        node = prune(node);
    }
    if node.role.is_none()
        && node.name.is_none()
        && node.value.is_none()
        && node.description.is_none()
        && node.children.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(node))
}

/// Convert one CDP `AXNode` into an [`AccessibilityNode`], recursing into its
/// `childIds`.
fn convert_node(
    raw: &Value,
    by_id: &HashMap<String, &Value>,
    interesting_only: bool,
) -> Result<AccessibilityNode> {
    let role = property_value(raw, "role").or_else(|| {
        raw.get("role")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    // CDP carries `name` and `description` as top-level `AxValue` fields (same
    // shape as `role`: `{ type, value }`), so fall back to the top-level value
    // when they aren't present in the `properties` array.
    let name = property_value(raw, "name").or_else(|| {
        raw.get("name")
            .and_then(|n| n.get("value"))
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let description = property_value(raw, "description").or_else(|| {
        raw.get("description")
            .and_then(|d| d.get("value"))
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let value = raw
        .get("value")
        .and_then(|v| v.get("value"))
        .cloned()
        .filter(|v| !v.is_null());

    let checked = property_value(raw, "checked").filter(|s| s != "false");
    let expanded = property_value(raw, "expanded").map(|s| match s.as_str() {
        "true" => "expanded".to_string(),
        "false" => "collapsed".to_string(),
        other => other.to_string(),
    });
    let disabled = boolean_property(raw, "disabled");
    let focused = boolean_property(raw, "focused");
    let modal = boolean_property(raw, "modal");
    let multiline = boolean_property(raw, "multiline");
    let readonly = boolean_property(raw, "readonly");
    let required = boolean_property(raw, "required");
    let selected = boolean_property(raw, "selected");
    let hidden = boolean_property(raw, "hidden")
        .or_else(|| property_value(raw, "visibility").map(|s| s == "hidden"));

    let mut children = Vec::new();
    if let Some(child_ids) = raw.get("childIds").and_then(|v| v.as_array()) {
        for cid in child_ids {
            if let Some(id) = cid.as_str() {
                if let Some(child_raw) = by_id.get(id) {
                    let mut child = convert_node(child_raw, by_id, interesting_only)?;
                    if interesting_only {
                        child = prune(child);
                    }
                    // After pruning, drop empty children entirely so they don't
                    // pad the tree.
                    let empty = child.role.is_none()
                        && child.name.is_none()
                        && child.value.is_none()
                        && child.description.is_none()
                        && child.children.is_empty();
                    if !empty {
                        children.push(child);
                    }
                }
            }
        }
    }

    Ok(AccessibilityNode {
        role,
        name,
        value,
        description,
        checked,
        disabled,
        expanded,
        focused,
        modal,
        multiline,
        readonly,
        required,
        selected,
        hidden,
        children,
    })
}

/// Read a string-valued property from a node's `properties` array.
///
/// CDP shapes properties as `[{ name, value: { type, value } }, ...]`.
fn property_value(node: &Value, name: &str) -> Option<String> {
    let props = node.get("properties").and_then(|v| v.as_array())?;
    for p in props {
        let matches = p
            .get("name")
            .and_then(|v| v.as_str())
            .map(|n| n == name)
            .unwrap_or(false);
        if matches {
            return p
                .get("value")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_str())
                .map(String::from);
        }
    }
    None
}

/// Read a boolean-valued property from a node's `properties` array.
fn boolean_property(node: &Value, name: &str) -> Option<bool> {
    let props = node.get("properties").and_then(|v| v.as_array())?;
    for p in props {
        let matches = p
            .get("name")
            .and_then(|v| v.as_str())
            .map(|n| n == name)
            .unwrap_or(false);
        if matches {
            return p
                .get("value")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_bool());
        }
    }
    None
}

/// Drop an "interesting_only"-filtered node that carries no meaningful
/// payload, returning an empty node; the caller then discards empty nodes.
fn prune(node: AccessibilityNode) -> AccessibilityNode {
    // Keep nodes that have any user-facing property or surviving children.
    let has_payload = node.role.is_some()
        || node.name.is_some()
        || node.value.is_some()
        || node.description.is_some()
        || node.checked.is_some()
        || node.expanded.is_some()
        || node.disabled == Some(true)
        || node.focused == Some(true)
        || node.selected == Some(true);
    if has_payload || !node.children.is_empty() {
        node
    } else {
        AccessibilityNode::default()
    }
}
