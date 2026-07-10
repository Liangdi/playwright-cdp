//! `Video` — page video capture via CDP `Page.startScreencast`.
//!
//! Playwright records a real video file (muxed by a bundled encoder). CDP has
//! **no native video muxing**: the closest primitive is `Page.startScreencast`,
//! which streams individual captured frames as base64-encoded images through
//! the `Page.screencastFrame` event.
//!
//! This module implements capture on top of that primitive. [`Video::start`]
//! subscribes to `Page.screencastFrame` and writes each incoming frame to the
//! configured output `path`; [`Video::stop`] ends the cast.
//!
//! ## Scope / caveats
//!
//! - The output is **a sequence of captured frames, not a muxed video file**.
//!   By default each frame is written as its own image file
//!   (`<path>/frame-NNNNN.png` when `path` is a directory). When `path` points
//!   at a single file, frames are appended as raw image chunks (a "filmstrip"),
//!   which is still *not* a playable video.
//! - Producing an actual video (`.webm`/`.mp4`) requires piping these frames to
//!   an external encoder (e.g. ffmpeg); that encoding step is intentionally
//!   **out of scope** here. Callers wanting a true video should stop the cast,
//!   then feed the captured frames to an external encoder.
//! - [`Video::path`] returns the configured output path (Playwright shape).
//!
//! The handle is cheaply cloneable (`Arc<Inner>`); clones share recording
//! state, so any clone can `stop()` the in-progress cast.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Options for [`Video::start`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct VideoStartOptions {
    /// Image format of each screencast frame. CDP supports `"jpeg"` and
    /// `"png"`; defaults to `"jpeg"` (smaller on the wire — better for capture
    /// throughput at the cost of lossy frames).
    pub format: Option<String>,
    /// Image compression quality (0–100). Only meaningful for `"jpeg"`.
    pub quality: Option<i64>,
    /// Capture every Nth frame. `1` = every frame. Defaults to `1`.
    pub every_nth_frame: Option<i64>,
    /// Maximum frame width. `None` lets CDP choose.
    pub max_width: Option<i64>,
    /// Maximum frame height. `None` lets CDP choose.
    pub max_height: Option<i64>,
}

impl VideoStartOptions {
    pub fn format(mut self, v: impl Into<String>) -> Self {
        self.format = Some(v.into());
        self
    }
    pub fn quality(mut self, v: i64) -> Self {
        self.quality = Some(v);
        self
    }
    pub fn every_nth_frame(mut self, v: i64) -> Self {
        self.every_nth_frame = Some(v);
        self
    }
    pub fn max_width(mut self, v: i64) -> Self {
        self.max_width = Some(v);
        self
    }
    pub fn max_height(mut self, v: i64) -> Self {
        self.max_height = Some(v);
        self
    }
}

/// A page video capture handle.
///
/// See the [module docs](self) for the important distinction between captured
/// frames and a muxed video file.
#[derive(Clone)]
pub struct Video {
    inner: Arc<VideoInner>,
}

struct VideoInner {
    /// The page-level CDP session (Page domain).
    session: Arc<CdpSession>,
    /// Configured output path. `None` until set via [`Video::with_path`] or
    /// [`Video::start`].
    path: Mutex<Option<PathBuf>>,
    /// Whether a cast is currently in progress. Guards against double-start /
    /// stop-without-start.
    recording: AtomicBool,
    /// Number of frames captured during the current (or most recent) cast.
    /// Arc-wrapped so the spawned frame-writer task can observe the live count.
    frame_count: Arc<AtomicU64>,
}

impl Video {
    pub(crate) fn new(session: Arc<CdpSession>) -> Self {
        Self {
            inner: Arc::new(VideoInner {
                session,
                path: Mutex::new(None),
                recording: AtomicBool::new(false),
                frame_count: Arc::new(AtomicU64::new(0)),
            }),
        }
    }

    /// Like [`new`](Self::new), but pre-records the output `path` (Playwright
    /// configures the video path up front, before any page interaction).
    pub(crate) fn with_path(session: Arc<CdpSession>, path: PathBuf) -> Self {
        let v = Self::new(session);
        *v.inner.path.lock() = Some(path);
        v
    }

    /// The configured output path, if any.
    ///
    /// Mirrors Playwright's `video.path()`. Here it returns the path as soon as
    /// it has been set (via [`with_path`](Self::with_path) or
    /// [`start`](Self::start)).
    pub fn path(&self) -> Option<PathBuf> {
        self.inner.path.lock().clone()
    }

    /// Begin capturing screencast frames.
    ///
    /// Subscribes to `Page.screencastFrame` **before** issuing
    /// `Page.startScreencast` so the first frame is not missed, then spawns a
    /// writer task that decodes each frame's base64 image and persists it.
    /// Frames are written under `path` (see [module docs](self) for the output
    /// shape). If `path` is `None`, capture still runs and counts frames via
    /// [`frame_count`](Self::frame_count), but nothing is persisted.
    pub async fn start(&self, options: Option<VideoStartOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();

        if self.inner.recording.swap(true, Ordering::AcqRel) {
            return Err(Error::InvalidArgument(
                "Video::start called while already recording".into(),
            ));
        }
        self.inner.frame_count.store(0, Ordering::Release);

        // Build the CDP params.
        let mut params = json!({});
        if let Some(fmt) = &opts.format {
            params["format"] = json!(fmt);
        }
        if let Some(q) = opts.quality {
            params["quality"] = json!(q);
        }
        if let Some(n) = opts.every_nth_frame {
            params["everyNthFrame"] = json!(n);
        }
        if let Some(w) = opts.max_width {
            params["maxWidth"] = json!(w);
        }
        if let Some(h) = opts.max_height {
            params["maxHeight"] = json!(h);
        }

        // Subscribe BEFORE Page.startScreencast to avoid missing the first frame.
        let mut rx = self.inner.session.subscribe();

        // Spawn the frame writer, then start the cast. Order matters: the
        // writer must be draining the bus when the first frame arrives.
        let path = self.inner.path.lock().clone();
        let frame_count = Arc::clone(&self.inner.frame_count);
        let session_for_ack = Arc::clone(&self.inner.session);
        tokio::spawn(async move {
            let engine = base64::engine::general_purpose::STANDARD;
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) if ev.method == "Page.screencastFrame" => ev,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                };

                // Acknowledge the frame so CDP sends the next one. `sessionId`
                // here is the screencast session id, not a CDP target session.
                if let Some(id) = ev.params.get("sessionId").and_then(|v| v.as_i64()) {
                    let _ = session_for_ack
                        .send("Page.screencastFrameAck", json!({ "sessionId": id }))
                        .await;
                }

                // A frame with `metadata: true` carries layout/offset info and
                // no pixel `data`; skip persistence for it.
                let is_metadata = ev
                    .params
                    .get("metadata")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if !is_metadata {
                    if let Some(data_str) = ev.params.get("data").and_then(|v| v.as_str()) {
                        if let Ok(bytes) = engine.decode(data_str) {
                            let idx = frame_count.fetch_add(1, Ordering::Relaxed);
                            // Persisting a single frame failing should not abort
                            // the whole cast; log via the ignored result.
                            let _ = write_frame(&path, idx, &bytes).await;
                        }
                    }
                }
            }
        });

        self.inner
            .session
            .send("Page.startScreencast", params)
            .await
            .map(|_: Value| ())?;
        Ok(())
    }

    /// Stop an in-progress cast.
    ///
    /// Sends `Page.stopScreencast`. The writer task winds down when the session
    /// stops emitting `Page.screencastFrame`. The configured `path` is retained
    /// so [`path`](Self::path) stays valid after recording.
    pub async fn stop(&self) -> Result<()> {
        if !self.inner.recording.swap(false, Ordering::AcqRel) {
            return Err(Error::InvalidArgument(
                "Video::stop called without a preceding Video::start".into(),
            ));
        }
        self.inner
            .session
            .send("Page.stopScreencast", json!({}))
            .await
            .map(|_: Value| ())?;
        Ok(())
    }

    /// Number of frames captured during the current (or most recent) cast.
    pub fn frame_count(&self) -> u64 {
        self.inner.frame_count.load(Ordering::Relaxed)
    }

    /// Whether a cast is currently in progress.
    pub fn is_recording(&self) -> bool {
        self.inner.recording.load(Ordering::Acquire)
    }
}

/// Persist a single captured frame.
///
/// If `base` is a directory (or absent), frames are written as
/// `frame-NNNNN.<ext>` inside it. If `base` is a single file, frames are
/// appended to it as concatenated image chunks (a "filmstrip"), which is
/// **not** a playable video — see the [module docs](self).
async fn write_frame(base: &Option<PathBuf>, index: u64, bytes: &[u8]) -> std::io::Result<()> {
    let Some(base) = base else {
        // No path configured — capture is in-memory only.
        return Ok(());
    };

    if base.is_dir() {
        let ext = sniff_ext(bytes);
        let name = format!("frame-{index:05}.{ext}");
        tokio::fs::write(base.join(name), bytes).await
    } else {
        // Append to a single file (create the parent dir if needed).
        if let Some(parent) = base.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(base)
            .await?;
        f.write_all(bytes).await?;
        Ok(())
    }
}

/// Sniff a frame's file extension from its magic bytes.
fn sniff_ext(bytes: &[u8]) -> &'static str {
    // PNG: 89 50 4E 47; JPEG: FF D8 FF.
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else {
        "bin"
    }
}
