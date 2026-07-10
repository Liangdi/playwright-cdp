//! JS dialogs (alert/confirm) + file downloads.
//!
//! Registers `page.on_dialog` (auto-accept) and `page.on_download` handlers
//! before triggering them, then verifies the dialog was accepted and the
//! downloaded file content matches what the server sent.
//!
//! Run: `cargo run --example dialogs_downloads`

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::{Download, Playwright};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A minimal loop-accepting, path-aware HTTP server: serves the demo HTML at
/// `/` and a fixed-body file at `/file`. Returns the base URL.
async fn serve() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { break };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let req_line = String::from_utf8_lossy(&buf);
                let path = req_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let (ctype, body) = if path.starts_with("/file") {
                    (
                        "application/octet-stream",
                        "DOWNLOAD-CONTENT" as &'static str,
                    )
                } else {
                    (
                        "text/html",
                        "<!doctype html><html><body>\
                         <button id=\"alert\" onclick=\"alert('hello from page')\">alert</button>\
                         <button id=\"confirm\" onclick=\"window.__conf = confirm('sure?')\">confirm</button>\
                         <a id=\"dl\" href=\"/file\" download>download</a>\
                         </body></html>" as &'static str,
                    )
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
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

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let base = serve().await;
    println!("serving dialog/download demo page at {base}");

    let page = browser.new_page().await?;

    // --- dialogs: register the auto-accept handler BEFORE navigating ---
    page.on_dialog(|d| {
        println!("dialog: kind={} message={}", d.kind(), d.message());
        async move {
            d.accept(None).await.ok();
        }
    });

    // --- downloads: store the Download handle in a shared slot so main can
    //     read it after the click fires. ---
    let captured: Arc<Mutex<Option<Download>>> = Arc::new(Mutex::new(None));
    let captured_handler = Arc::clone(&captured);
    page.on_download(move |dl| {
        println!("download: url={} name={}", dl.url(), dl.suggested_filename());
        let slot = Arc::clone(&captured_handler);
        async move {
            *slot.lock().await = Some(dl);
        }
    });

    println!("navigating to {base} (handlers armed)...");
    page.goto(&base, None).await?;

    // --- alert: clicking auto-triggers alert(); the handler accepts it. ---
    println!("\nclicking #alert (auto-accepted by on_dialog)...");
    page.locator("#alert").click(None).await?;
    println!("alert dialog handled without blocking the click");

    // --- confirm: clicking sets window.__conf to true (accepted). ---
    println!("\nclicking #confirm (auto-accepted by on_dialog)...");
    page.locator("#confirm").click(None).await?;
    let conf: bool = page.evaluate("window.__conf").await?;
    println!("window.__conf after confirm: {conf}");
    assert!(conf, "confirm should have been accepted -> true");

    // --- download: clicking #dl fetches /file; on_download stores the handle. ---
    println!("\nclicking #dl to start the download...");
    page.locator("#dl").click(None).await?;

    // Poll the shared slot until the handler dropped the Download in.
    let dl = {
        let mut guard = captured.lock().await;
        let mut waited = 0u64;
        while guard.is_none() {
            drop(guard);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            waited += 50;
            if waited > 5000 {
                anyhow::bail!("timed out waiting for on_download to fire");
            }
            guard = captured.lock().await;
        }
        guard.take().unwrap()
    };

    tokio::fs::create_dir_all("examples_out").await?;
    let saved = dl.save_as("examples_out/dl.txt").await?;
    println!("saved download to: {}", saved.display());
    let content = tokio::fs::read_to_string(&saved).await?;
    println!("downloaded file content: {content}");
    assert_eq!(content, "DOWNLOAD-CONTENT");

    println!("\nall dialog + download steps completed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
