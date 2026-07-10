//! Accessibility-tree snapshot serialization into Playwright's aria-snapshot
//! YAML-ish format.
//!
//! Given a flat list of CDP [`Accessibility` domain](https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/)
//! nodes (as returned by `Accessibility.getFullAXTree` or
//! `Accessibility.getPartialAXTree`), this module rebuilds the parent/child
//! tree, filters out uninteresting/presentational nodes (lifting their
//! interesting descendants up to the nearest kept ancestor), and serializes
//! the result into Playwright's aria-snapshot format — a YAML-ish, 2-space
//! indented outline, e.g.:
//!
//! ```text
//! - heading "Welcome" [level=1]:
//!   - paragraph: Some text here
//! - button "Submit"
//! ```
//!
//! This aims for Playwright's format but is **not** guaranteed byte-identical:
//! the exact attribute set, generic-container handling, and node filtering may
//! differ in edge cases. The goal is a faithful, readable snapshot.

use serde_json::Value;
use std::collections::HashMap;

/// Roles considered "interesting" enough to render. Matched case-insensitively
/// against the CDP `role.value`. Everything else is dropped (with descendants
/// lifted to the nearest kept ancestor), unless it is `none`/`presentation`
/// with a non-empty name.
const INTERESTING_ROLES: &[&str] = &[
    "alert", "alertdialog", "application", "article", "banner", "blockquote", "button", "caption",
    "cell", "checkbox", "columnheader", "combobox", "complementary", "contentinfo", "date",
    "definition", "dialog", "directory", "document", "figure", "form", "generic", "grid",
    "gridcell", "group", "heading", "img", "image", "link", "list", "listbox", "listitem", "main",
    "menu", "menubar", "menuitem", "menuitemcheckbox", "menuitemradio", "meter", "navigation",
    "note", "option", "paragraph", "progressbar", "radio", "radiogroup", "region", "row",
    "rowgroup", "rowheader", "scrollbar", "search", "searchbox", "separator", "slider",
    "spinbutton", "status", "switch", "tab", "table", "tablist", "tabpanel", "term", "textbox",
    "time", "timer", "toolbar", "tooltip", "tree", "treegrid", "treeitem",
];

/// A parsed, in-memory AX node used while building/serializing the tree.
struct Node {
    role: String,
    name: String,
    /// Ordered attribute strings already formatted for output, e.g.
    /// `level=1`, `checked`, `disabled`. Empty if none.
    attrs: Vec<String>,
    /// Children, populated during tree-building. After the lift pass, these
    /// are only the kept (interesting) descendants.
    children: Vec<usize>,
}

fn is_interesting_role(role: &str) -> bool {
    let r = role.to_ascii_lowercase();
    INTERESTING_ROLES.iter().any(|&x| x == r)
}

/// Pull a property value (the inner `.value.value`) out of an AxNode's
/// `properties` array by name. Returns the raw JSON value.
fn prop_value<'a>(props: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    let arr = props.and_then(|v| v.as_array())?;
    for p in arr {
        if p.get("name").and_then(|v| v.as_str()) == Some(name) {
            return p.get("value").and_then(|v| v.get("value"));
        }
    }
    None
}

/// Extract a boolean property (defaulting to `false`).
fn prop_bool(props: Option<&Value>, name: &str) -> bool {
    prop_value(props, name).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Parse one CDP AxNode into our intermediate `Node`. Returns `None` for nodes
/// that should be skipped entirely (ignored, or uninteresting role without a
/// name to redeem it).
fn parse_node(raw: &Value) -> Option<Node> {
    let ignored = raw.get("ignored").and_then(|v| v.as_bool()).unwrap_or(false);
    if ignored {
        return None;
    }

    let role = raw
        .get("role")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = raw
        .get("name")
        .and_then(|n| n.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let props = raw.get("properties");

    let role_lc = role.to_ascii_lowercase();

    // none/presentation are skipped unless they carry a name.
    if (role_lc == "none" || role_lc == "presentation") && name.trim().is_empty() {
        return None;
    }

    // Unknown/uninteresting role with no name and no interesting descendants is
    // dropped; descendants are lifted during tree-building. (We still parse it
    // so its children can be re-homed — but mark it non-kept via a sentinel:
    // we simply don't add it to the output list when it has no name AND isn't
    // interesting. To keep tree-building simple we parse everything and filter
    // at serialization time.)
    let attrs = build_attrs(&role_lc, name.as_str(), props);

    Some(Node {
        role,
        name,
        attrs,
        children: Vec::new(),
    })
}

/// Build the formatted attribute list for a node.
fn build_attrs(role_lc: &str, _name: &str, props: Option<&Value>) -> Vec<String> {
    let mut attrs = Vec::new();

    // level — only meaningful for structural hierarchy roles. Playwright emits
    // it for headings (and tree/grid levels); emitting it on every node that
    // happens to carry a `level` property (e.g. listitems) is noise.
    if matches!(
        role_lc,
        "heading" | "row" | "treeitem" | "listitem" | "tab" | "option" | "cell"
    ) {
        if let Some(lvl) = prop_value(props, "level").and_then(|v| v.as_i64()) {
            // Only emit for headings unconditionally; others only when > 1.
            if role_lc == "heading" || lvl > 1 {
                attrs.push(format!("level={lvl}"));
            }
        }
    }

    // Boolean attributes — only emit when true.
    let mut bool_attr = |n: &str| {
        if prop_bool(props, n) {
            attrs.push(n.to_string());
        }
    };
    bool_attr("disabled");
    bool_attr("expanded");
    bool_attr("collapsed");
    bool_attr("selected");
    bool_attr("readonly");
    bool_attr("required");
    bool_attr("focused");
    bool_attr("modal");
    bool_attr("multiline");
    bool_attr("hidden");

    // checked: true -> [checked], "mixed" -> [mixed]
    if let Some(c) = prop_value(props, "checked") {
        match c.as_str() {
            Some("mixed") => attrs.push("mixed".to_string()),
            _ => {
                if c.as_bool().unwrap_or(false) {
                    attrs.push("checked".to_string());
                }
            }
        }
    }
    // pressed: same scheme as checked.
    if let Some(p) = prop_value(props, "pressed") {
        match p.as_str() {
            Some("mixed") => {
                if !attrs.contains(&"mixed".to_string()) {
                    attrs.push("mixed".to_string());
                }
            }
            _ => {
                if p.as_bool().unwrap_or(false) {
                    attrs.push("pressed".to_string());
                }
            }
        }
    }

    // Valued attributes for sliders/ranges (optional but cheap to include).
    match role_lc {
        "slider" | "spinbutton" | "scrollbar" | "meter" | "progressbar" => {
            if let Some(vmin) = numeric_value(props, "valuemin") {
                attrs.push(format!("valuemin={vmin}"));
            }
            if let Some(vmax) = numeric_value(props, "valuemax") {
                attrs.push(format!("valuemax={vmax}"));
            }
            if let Some(vt) = prop_value(props, "valuetext").and_then(|v| v.as_str()) {
                attrs.push(format!("valuetext={vt}"));
            }
        }
        _ => {}
    }

    attrs
}

/// Pull a numeric property, rendering integers without a trailing `.0`.
fn numeric_value(props: Option<&Value>, name: &str) -> Option<String> {
    let v = prop_value(props, name)?;
    if let Some(i) = v.as_i64() {
        return Some(i.to_string());
    }
    v.as_f64().map(|f| format!("{f}"))
}

/// Serialize a flat CDP AX-node array into the aria-snapshot YAML format.
///
/// `nodes` is the `nodes` array from a `getFullAXTree` / `getPartialAXTree`
/// response. When `root_backend_dom_node_id` is `Some`, only the subtree
/// rooted at the AX node whose `backendDOMNodeId` matches is rendered (used
/// for element-scoped snapshots); otherwise the whole forest is rendered.
pub(crate) fn serialize(nodes: &[Value], root_backend_dom_node_id: Option<i64>) -> String {
    // Parse every node and record its CDP nodeId + parentId so we can rebuild
    // the tree. We keep a map from CDP nodeId -> index into `parsed`.
    let mut parsed: Vec<Node> = Vec::with_capacity(nodes.len());
    // CDP nodeId -> index in `parsed`.
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    // (child_index, parent_nodeId_string)
    let mut parent_links: Vec<(usize, Option<String>)> = Vec::new();

    for raw in nodes {
        let Some(node) = parse_node(raw) else {
            continue;
        };
        let idx = parsed.len();
        let id = raw
            .get("nodeId")
            .and_then(|v| v.as_str())
            .map(String::from);
        if let Some(id) = &id {
            id_to_idx.insert(id.clone(), idx);
        }
        let parent = raw
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(String::from);
        parent_links.push((idx, parent.or(id)));
        parsed.push(node);
    }

    // backend DOM node id per parsed index, for element-scoped roots.
    let be_id: Vec<Option<i64>> = nodes
        .iter()
        .filter_map(|raw| {
            // Only nodes that survived parse_node (non-ignored) are in `parsed`,
            // so mirror that filter here.
            if parse_node(raw).is_some() {
                Some(
                    raw.get("backendDOMNodeId")
                        .and_then(|v| v.as_i64()),
                )
            } else {
                None
            }
        })
        .collect();

    // Wire up children. A node's children come from `childIds` if present,
    // otherwise inferred from parent links. Prefer `childIds` (more reliable).
    // First record every (parent_idx, child_idx) edge.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for raw in nodes {
        let Some(child_id) = raw.get("nodeId").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(&child_idx) = id_to_idx.get(child_id) else {
            continue;
        };
        if let Some(child_ids) = raw.get("childIds").and_then(|v| v.as_array()) {
            for cid in child_ids {
                if let Some(cid_s) = cid.as_str() {
                    if let Some(&cidx) = id_to_idx.get(cid_s) {
                        edges.push((child_idx, cidx));
                    }
                }
            }
        } else if let Some(pid) = raw.get("parentId").and_then(|v| v.as_str()) {
            if let Some(&pidx) = id_to_idx.get(pid) {
                edges.push((pidx, child_idx));
            }
        }
    }
    // (childIds may double-add via the parent-link fallback above; dedupe by
    // using a per-parent ordering and skipping repeats.)
    {
        let mut seen: HashMap<(usize, usize), ()> = HashMap::new();
        for (p, c) in edges {
            if p == c {
                continue;
            }
            if seen.insert((p, c), ()).is_none() {
                parsed[p].children.push(c);
            }
        }
    }

    // Snapshot the original (full) parent->children adjacency before the lift
    // pass rewrites `children`. Used to walk text descendants when computing
    // effective names for nodes whose own name is empty.
    let orig_children: Vec<Vec<usize>> = parsed.iter().map(|n| n.children.clone()).collect();

    // Determine the set of "kept" nodes (interesting role, or none/presentation
    // with a name — already enforced by parse_node for the latter). Everything
    // else is dropped and its kept descendants lifted to the nearest kept
    // ancestor. We do this with a single rewrite of children lists via a DFS.
    let kept: Vec<bool> = parsed
        .iter()
        .map(|n| is_kept(n))
        .collect();

    // If an element-scoped root is requested, find the matching AX node and
    // compute the set of indices within its subtree. Nodes outside the subtree
    // are dropped entirely; ancestors outside the subtree are not attached-to.
    let in_subtree: Option<Vec<bool>> = if let Some(root_be) = root_backend_dom_node_id {
        let root_idx = be_id.iter().position(|b| *b == Some(root_be));
        if let Some(root_idx) = root_idx {
            let mut mask = vec![false; parsed.len()];
            let mut stack = vec![root_idx];
            mask[root_idx] = true;
            while let Some(i) = stack.pop() {
                if let Some(kids) = orig_children.get(i) {
                    for &c in kids {
                        if !mask[c] {
                            mask[c] = true;
                            stack.push(c);
                        }
                    }
                }
            }
            Some(mask)
        } else {
            // No matching node — render nothing.
            return String::new();
        }
    } else {
        None
    };
    let in_subtree = in_subtree.unwrap_or_else(|| vec![true; parsed.len()]);
    let subtree_contains = |i: usize| in_subtree.get(i).copied().unwrap_or(false);

    let mut roots: Vec<usize> = Vec::new();
    // For each kept node, find its nearest kept ancestor and attach there (or
    // register as a root). Collect attachments first to avoid borrowing parsed
    // mutably while iterating.
    let mut attachments: Vec<(usize, usize)> = Vec::new();
    for idx in 0..parsed.len() {
        if !kept[idx] || !subtree_contains(idx) {
            continue;
        }
        match nearest_kept_ancestor(idx, &kept, &parent_links, &id_to_idx) {
            Some(parent) if subtree_contains(parent) => attachments.push((parent, idx)),
            _ => {
                if !roots.contains(&idx) {
                    roots.push(idx);
                }
            }
        }
    }
    for (parent, child) in attachments {
        if !parsed[parent].children.contains(&child) {
            parsed[parent].children.push(child);
        }
    }
    // Filter each node's children list to kept nodes only (the lift above
    // already attaches lifted descendants, but original child lists may contain
    // dropped nodes — prune them so they aren't rendered).
    for n in parsed.iter_mut() {
        n.children.retain(|&c| kept[c]);
    }

    // Effective-name pass: for kept nodes whose own accessible name is empty,
    // derive a name from the text of their non-rendered descendants
    // (StaticText/InlineTextBox). This mirrors how Playwright shows e.g.
    // `- listitem: First` even though CDP reports the listitem with an empty
    // name and the text in a child StaticText node.
    for idx in 0..parsed.len() {
        if !kept[idx] {
            continue;
        }
        if !parsed[idx].name.trim().is_empty() {
            continue;
        }
        if let Some(text) = collect_text(idx, &parsed, &orig_children) {
            let t = text.trim();
            if !t.is_empty() {
                parsed[idx].name = t.to_string();
            }
        }
    }

    let mut out = String::new();
    for (i, &root) in roots.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render(&parsed, root, 0, &mut out);
    }

    // Ensure trailing newline for a clean file-like output.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// A node is kept if its role is interesting, or it is none/presentation with a
/// non-empty name. (`generic` is in the interesting set, so plain containers
/// survive.)
fn is_kept(n: &Node) -> bool {
    let role_lc = n.role.to_ascii_lowercase();
    if is_interesting_role(&role_lc) {
        return true;
    }
    if (role_lc == "none" || role_lc == "presentation") && !n.name.trim().is_empty() {
        return true;
    }
    false
}

/// Collect text from a node's non-rendered descendants (text/structural nodes
/// that won't appear in the output). Walks the original tree, stopping at any
/// kept node (whose name belongs to it, not to `root`). Returns the aggregated,
/// whitespace-collapsed text, if any.
fn collect_text(
    root: usize,
    parsed: &[Node],
    orig_children: &[Vec<usize>],
) -> Option<String> {
    let mut out = String::new();
    let mut stack: Vec<usize> = orig_children
        .get(root)
        .cloned()
        .unwrap_or_default();
    while let Some(idx) = stack.pop() {
        let role_lc = parsed[idx].role.to_ascii_lowercase();
        if is_kept(&parsed[idx]) {
            // Don't descend into rendered children: their text belongs to them.
            continue;
        }
        let is_text = matches!(
            role_lc.as_str(),
            "statictext" | "inlinetextbox" | "text" | "caption"
        );
        if is_text {
            if !parsed[idx].name.is_empty() {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(&parsed[idx].name);
            }
            // A text node's children are inline boxes whose text duplicates the
            // text node's own name — do not descend.
            continue;
        }
        // Descend into this (non-kept) node's children, preserving document
        // order (stack pops from the end, so push in reverse).
        if let Some(kids) = orig_children.get(idx) {
            for &c in kids.iter().rev() {
                stack.push(c);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Walk parent links upward from `idx` to find the nearest ancestor that is
/// kept. Returns its index, or `None` if `idx` is effectively a root.
fn nearest_kept_ancestor(
    idx: usize,
    kept: &[bool],
    parent_links: &[(usize, Option<String>)],
    id_to_idx: &HashMap<String, usize>,
) -> Option<usize> {
    // Find this node's CDP parent id via parent_links.
    let mut cur_id = parent_links
        .iter()
        .find(|(i, _)| *i == idx)
        .and_then(|(_, pid)| pid.clone());
    while let Some(pid_str) = cur_id {
        let Some(&pidx) = id_to_idx.get(&pid_str) else { return None };
        if kept[pidx] {
            return Some(pidx);
        }
        // climb up
        cur_id = parent_links
            .iter()
            .find(|(i, _)| *i == pidx)
            .and_then(|(_, pid)| pid.clone());
    }
    None
}

/// Escape a name for embedding inside double quotes: `\` and `"`.
fn escape_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// Render `idx` (and its kept descendants) into `out` at `depth` indent.
fn render(parsed: &[Node], idx: usize, depth: usize, out: &mut String) {
    let node = &parsed[idx];
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push_str("- ");
    out.push_str(&node.role.to_ascii_lowercase());

    if !node.name.is_empty() {
        out.push_str(" \"");
        out.push_str(&escape_name(&node.name));
        out.push('"');
    }

    if !node.attrs.is_empty() {
        out.push_str(" [");
        out.push_str(&node.attrs.join(" "));
        out.push(']');
    }

    let has_children = !node.children.is_empty();
    if has_children {
        out.push(':');
    }
    out.push('\n');

    for &child in &node.children {
        render(parsed, child, depth + 1, out);
    }
}
