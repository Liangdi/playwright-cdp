//! WebSocket capture demo: spins up a local WS echo server, connects from the
//! page, and verifies `page.on_websocket` captures the connection and its
//! frames.
//!
//! Run: `cargo run --example websocket_example`

use futures_util::{SinkExt, StreamExt};
use playwright_cdp::options::LaunchOptions;
use playwright_cdp::{Playwright, WebSocket};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::accept_async;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    let ws_url = ws_echo_server().await;
    println!("ws echo server at {ws_url}");

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;

    let captured = Arc::new(Mutex::new(None::<WebSocket>));
    let cap = Arc::clone(&captured);
    page.on_websocket(move |ws| {
        let c = Arc::clone(&cap);
        async move {
            *c.lock().await = Some(ws);
        }
    });

    // Connect from the page and send a frame on open. The IIFE returns
    // undefined; the side effect (connection + send) is what we test.
    let expr = format!(
        r#"(() => {{ const w = new WebSocket({ws_url:?}); window.__ws = w; w.onopen = () => w.send("ping"); }})()"#
    );
    let _: serde_json::Value = page.evaluate(&expr).await?;

    // Wait for handshake + echo round-trip.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let ready: i64 = page
        .evaluate("window.__ws ? window.__ws.readyState : -1")
        .await?;
    println!("ws readyState={ready} (0=connecting,1=open,2=closing,3=closed)");

    let ws = captured.lock().await.clone().expect("on_websocket did not fire");
    // The browser normalizes the WS url with a trailing '/', so compare by prefix.
    assert!(
        ws.url().starts_with(&ws_url),
        "captured WebSocket url should match (got {})",
        ws.url()
    );
    println!("captured ws url={} handshake.status={:?}", ws.url(), ws.handshake().status);
    assert_eq!(
        ws.handshake().status,
        Some(101),
        "handshake response status should be 101 (Switching Protocols)"
    );

    let frames = ws.frames();
    println!("captured {} frame(s)", frames.len());
    for f in &frames {
        println!("  frame: {:?}", f);
    }
    assert!(
        !frames.is_empty(),
        "expected at least one captured frame (sent 'ping' + echo)"
    );

    println!("PASS");
    browser.close().await?;
    Ok(())
}

/// A minimal WebSocket echo server on an ephemeral port; returns its `ws://`
/// URL. Echoes any text/binary frame back to the client.
async fn ws_echo_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let Ok(mut ws) = accept_async(stream).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    if msg.is_text() || msg.is_binary() {
                        if ws.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
            });
        }
    });
    format!("ws://127.0.0.1:{port}")
}
