//! Persistent context: storage survives across sessions via an on-disk
//! user-data-dir.
//!
//! `BrowserType::launch_persistent_context(dir)` launches Chromium against a
//! persistent on-disk user-data-dir, so cookies / localStorage persist to that
//! directory across runs. This example writes `localStorage` in one session,
//! closes, re-launches against the **same** dir + **same origin**, and reads
//! it back.
//!
//! The embedded HTTP server (copied from `tests/common/mod.rs`) is kept up
//! across both sessions so the origin (scheme+host+port) — and therefore the
//! localStorage partition — is identical.
//!
//! Run: `cargo run --example persistent_context`
//! Requires a Chrome/Chromium on the system, or set `CHROME_PATH`.

use std::time::Duration;

use playwright_cdp::Playwright;

/// A tiny loop-accepting HTTP server that serves `body` at `/` (and every
/// path) for every connection. Stays up until the runtime drops the task.
/// Returns its base URL. Mirrors `tests/common::http_server`.
async fn http_server(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let body = body;
            // Per-connection task so a stalled connection can't block others.
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });

    format!("http://127.0.0.1:{port}/")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    // A stable origin shared by both sessions. The same server task (and thus
    // the same port) must stay alive so localStorage partitions match.
    let base = http_server("<html><body><p>persistent-context demo</p></body></html>").await;
    println!("embedded server origin: {base}");

    // A temp user-data-dir. Cleaned at the end so re-runs start fresh.
    let dir = std::env::temp_dir().join("pw-cdp-persistent-demo");
    let _ = std::fs::remove_dir_all(&dir);
    println!("user-data-dir: {}", dir.display());

    // --- Session 1: write localStorage ---
    println!("\n[session 1] launching persistent context...");
    {
        let ctx = Playwright::launch()
            .await?
            .chromium()
            .launch_persistent_context(&dir)
            .await?;
        let page = ctx.new_page().await?;
        page.goto(&base, None).await?;
        let _: serde_json::Value = page
            .evaluate("localStorage.setItem('k','persisted')")
            .await?;
        let val: Option<String> = page.evaluate("localStorage.getItem('k')").await?;
        println!("[session 1] wrote localStorage['k'] = {val:?}");
        assert_eq!(val.as_deref(), Some("persisted"));
        ctx.close().await?;
        println!("[session 1] closed (user-data-dir stays on disk)");
    }

    // The on-disk profile must survive close().
    assert!(dir.exists(), "user-data-dir must persist after close");

    // --- Session 2: same dir, same origin -> read it back ---
    println!("\n[session 2] re-launching persistent context against the same dir...");
    {
        let ctx = Playwright::launch()
            .await?
            .chromium()
            .launch_persistent_context(&dir)
            .await?;
        let page = ctx.new_page().await?;
        page.goto(&base, None).await?;
        // Give the page a brief moment to load before probing storage.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let val: Option<String> = page
            .evaluate("localStorage.getItem('k')")
            .await?;
        println!("[session 2] read localStorage['k'] = {val:?}");
        assert_eq!(
            val.as_deref(),
            Some("persisted"),
            "localStorage must persist across sessions via the user-data-dir"
        );
        ctx.close().await?;
        println!("[session 2] closed");
    }

    // Clean up the temp profile.
    let _ = std::fs::remove_dir_all(&dir);
    println!("\npersistent storage survived across two browser sessions.");
    Ok(())
}
