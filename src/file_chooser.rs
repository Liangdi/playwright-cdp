//! `FileChooser` — file-chooser interception via the CDP `Page` domain.
//!
//! `Page::on_filechooser(handler)` enables chooser interception once
//! (`Page.setInterceptFileChooserDialog { enabled: true }`) and dispatches a
//! [`FileChooser`] for every `Page.fileChooserOpened` event. The handler
//! inspects the chooser ([`is_multiple`](FileChooser::is_multiple),
//! [`element`](FileChooser::element)) and accepts files via
//! [`set_files`](FileChooser::set_files) (`Page.handleFileChooser { action:
//! "accept", files: [...] }`).

use crate::element_handle::ElementHandle;
use crate::error::{Error, Result};
use crate::page::Page;
use serde_json::{json, Value};
use std::path::Path;

/// A file-chooser dialog opened by the page.
///
/// Produced by [`Page::on_filechooser`](Page::on_filechooser). Call
/// [`set_files`](FileChooser::set_files) to accept the chooser with the given
/// server-side file path(s).
#[derive(Clone)]
pub struct FileChooser {
    page: Page,
    /// The `<input type=file>` that triggered the chooser, if one is known
    /// (absent for choosers opened via `HTMLInputElement.showOpenFilePicker`
    /// without a backing element). Resolved lazily via `DOM.resolveNode`.
    backend_node_id: Option<i64>,
    multiple: bool,
}

impl FileChooser {
    pub(crate) fn new(page: Page, backend_node_id: Option<i64>, multiple: bool) -> Self {
        Self {
            page,
            backend_node_id,
            multiple,
        }
    }

    /// The owning page.
    pub fn page(&self) -> Page {
        self.page.clone()
    }

    /// `true` if the chooser allows selecting multiple files (`selectMultiple`).
    pub fn is_multiple(&self) -> bool {
        self.multiple
    }

    /// The `<input type=file>` element that triggered the chooser, if any.
    ///
    /// Resolves the CDP `backendNodeId` from the event to a remote object id via
    /// `DOM.resolveNode`, then wraps it in an [`ElementHandle`]. Returns `None`
    /// if the event carried no `backendNodeId` (no backing element).
    pub async fn element(&self) -> Result<Option<ElementHandle>> {
        let backend_node_id = match self.backend_node_id {
            Some(id) => id,
            None => return Ok(None),
        };
        let resp = self
            .page
            .session()
            .send("DOM.resolveNode", json!({ "backendNodeId": backend_node_id }))
            .await?;
        // The remote object is under `object.objectId`.
        let object_id = resp
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::ProtocolError("DOM.resolveNode missing object.objectId".into())
            })?;
        Ok(Some(ElementHandle::new(self.page.clone(), object_id.to_string())))
    }

    /// Accept the chooser, uploading the given server-side file path(s).
    ///
    /// `files` are absolute filesystem paths Chrome can read directly (not file
    /// contents). Sets the files on the chooser's `<input type=file>` (resolved
    /// from the event's `backendNodeId`) via `DOM.setFileInputFiles`, which is
    /// supported across Chrome versions.
    pub async fn set_files(&self, files: &[impl AsRef<Path>]) -> Result<()> {
        let backend_node_id = self.backend_node_id.ok_or_else(|| {
            Error::ProtocolError("file chooser has no backing input element".into())
        })?;
        let paths: Vec<String> = files
            .iter()
            .map(|p| p.as_ref().to_string_lossy().into_owned())
            .collect();
        self.page
            .session()
            .send(
                "DOM.setFileInputFiles",
                json!({ "backendNodeId": backend_node_id, "files": paths }),
            )
            .await
            .map(|_: Value| ())
    }
}
