//! `Download` — file-download capture via the CDP `Page` download events.
//!
//! `Page::on_download(handler)` enables downloads to a temp directory
//! (`Page.setDownloadBehavior` `allow`) and dispatches a [`Download`] for every
//! `Page.downloadWillBegin` event. Progress (`Page.downloadProgress`) updates
//! the per-guid shared state so `path()`/`save_as()` can await completion.

use crate::error::{Error, Result};
use crate::page::Page;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Internal lifecycle of a download, shared between the listener task and the
/// [`Download`] handle so async progress events are visible to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadState {
    InProgress,
    Completed,
    Canceled,
}

#[derive(Clone)]
pub(crate) struct DownloadStateCell {
    pub state: Arc<Mutex<DownloadState>>,
}

impl DownloadStateCell {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(DownloadState::InProgress)),
        }
    }
}

impl Default for DownloadStateCell {
    fn default() -> Self {
        Self::new()
    }
}

/// A file download initiated by the page.
///
/// Produced by [`Page::on_download`](Page::on_download). The download lands in
/// a per-page temp directory; call [`path`](Download::path) or
/// [`save_as`](Download::save_as) to retrieve it.
#[derive(Clone)]
pub struct Download {
    inner: Arc<DownloadInner>,
}

struct DownloadInner {
    url: String,
    suggested_filename: String,
    #[allow(dead_code)]
    guid: String,
    state: DownloadStateCell,
    /// The temp directory Chrome writes downloads into.
    download_path: PathBuf,
    page: Page,
}

impl Download {
    pub(crate) fn new(
        url: String,
        suggested_filename: String,
        guid: String,
        state: DownloadStateCell,
        download_path: PathBuf,
        page: Page,
    ) -> Self {
        Self {
            inner: Arc::new(DownloadInner {
                url,
                suggested_filename,
                guid,
                state,
                download_path,
                page,
            }),
        }
    }

    /// The URL being downloaded.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// The page that owns this download.
    ///
    /// Cheap clone — [`Page`] is `Arc`-backed, so the returned handle shares the
    /// page's state with the caller.
    pub fn page(&self) -> Page {
        self.inner.page.clone()
    }

    /// The filename suggested by the server (`suggestedFilename`).
    pub fn suggested_filename(&self) -> &str {
        &self.inner.suggested_filename
    }

    /// A failure reason, if the download was canceled. `None` otherwise.
    pub fn failure(&self) -> Option<&str> {
        if *self.inner.state.state.lock() == DownloadState::Canceled {
            Some("download canceled")
        } else {
            None
        }
    }

    /// Wait (up to ~30s) for the download to reach a terminal state, then
    /// return the on-disk path if completed. Falls back to the filesystem: some
    /// Chrome builds omit the `Completed` `downloadProgress` event, so if the
    /// final file (or its `.crdownload` temp) appears on disk and its size
    /// stabilizes, we treat the download as done.
    async fn await_completion(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match *self.inner.state.state.lock() {
                DownloadState::Completed => return Ok(()),
                DownloadState::Canceled => {
                    return Err(Error::ProtocolError("download was canceled".into()))
                }
                DownloadState::InProgress => {}
            }
            // Filesystem fallback: a file landed on disk even without an event.
            if let Some(p) = self.find_stable_file().await {
                // If Chrome left it as `*.crdownload`, rename to the final name
                // so subsequent path()/save_as() reads resolve it.
                if p.extension().and_then(|e| e.to_str()) == Some("crdownload") {
                    let final_path = self
                        .inner
                        .download_path
                        .join(&self.inner.suggested_filename);
                    let _ = tokio::fs::rename(&p, &final_path).await;
                }
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout(
                    "timed out waiting for download to complete".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Look in the download dir for the final file or an in-progress
    /// `.crdownload` temp whose size has stopped growing.
    async fn find_stable_file(&self) -> Option<PathBuf> {
        let final_path = self.inner.download_path.join(&self.inner.suggested_filename);
        if self.size_stable(&final_path).await {
            return Some(final_path);
        }
        // Scan for `*.crdownload` files (Chrome's in-progress temp name).
        let mut entries = tokio::fs::read_dir(&self.inner.download_path).await.ok()?;
        while let Ok(Some(e)) = entries.next_entry().await {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("crdownload")
                && self.size_stable(&p).await
            {
                return Some(p);
            }
        }
        None
    }

    /// True if the file size stops changing across two reads ~150ms apart.
    async fn size_stable(&self, path: &Path) -> bool {
        let s1 = tokio::fs::metadata(path).await.map(|m| m.len()).ok();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let s2 = tokio::fs::metadata(path).await.map(|m| m.len()).ok();
        s1.is_some() && s1 == s2
    }

    /// The on-disk path of the completed download, or `None` if it failed.
    ///
    /// Waits for the download to finish; once `Completed`, polls briefly for
    /// the file to appear (Chrome renames the `.crdownload` temp on finish).
    pub async fn path(&self) -> Result<Option<PathBuf>> {
        if self.await_completion().await.is_err() {
            return Ok(None);
        }
        let target = self.inner.download_path.join(&self.inner.suggested_filename);
        // Chrome renames the in-progress .crdownload file on completion; allow
        // a brief window for the rename to land on disk.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if tokio::fs::metadata(&target).await.is_ok() {
                return Ok(Some(target));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Some(target)); // best-effort; caller may still read it
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Copy the downloaded file to `to` and return the destination path.
    pub async fn save_as(&self, to: impl AsRef<Path>) -> Result<PathBuf> {
        self.await_completion().await?;
        let from = self.path().await?.ok_or_else(|| {
            Error::ProtocolError("download has no file to save (failed)".into())
        })?;
        let to = to.as_ref().to_path_buf();
        if let Some(parent) = to.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        tokio::fs::copy(&from, &to).await?;
        Ok(to)
    }

    /// Best-effort remove the downloaded file from the temp directory.
    pub async fn delete(&self) -> Result<()> {
        let target = self.inner.download_path.join(&self.inner.suggested_filename);
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Open the downloaded file as a read stream.
    pub async fn create_read_stream(&self) -> Result<tokio::fs::File> {
        self.await_completion().await?;
        let from = self.path().await?.ok_or_else(|| {
            Error::ProtocolError("download has no file to read (failed)".into())
        })?;
        Ok(tokio::fs::File::open(from).await?)
    }
}
