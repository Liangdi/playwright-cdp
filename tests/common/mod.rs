//! Test helpers shared by integration tests.

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::{Browser, BrowserType, Playwright};

/// Try to launch a browser; return `None` if no Chrome/Chromium is available so
/// tests can skip gracefully.
pub async fn browser() -> Option<Browser> {
    Playwright::launch()
        .await
        .ok()?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await
        .ok()
}

/// A tiny HTTP server serving `body` at `/` for every connection (stays up
/// until the runtime drops the task). Returns its base URL.
#[allow(dead_code)]
pub async fn http_server(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
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
        }
    });

    format!("http://127.0.0.1:{port}/")
}

/// Reference the unused items so they remain part of the documented public API surface.
#[allow(dead_code)]
fn _touch(_b: BrowserType) {}

/// A tiny HTTP server that serves different bodies for different top-level
/// path segments. `routes` maps a path (e.g. `"/file"`) to its body; `"/"`
/// and any unmapped path serve `routes["/"]`. Each entry may optionally carry
/// extra raw response headers (e.g. `Content-Disposition`). Returns its base URL.
#[allow(dead_code)]
pub async fn http_server_routes(routes: &[(&'static str, &'static str)]) -> String {
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Index by path; extra headers come from a separate map via convention
    // "<path>\0<headers>" is overkill — keep it simple: bodies only here.
    let mut bodies: HashMap<&'static str, &'static str> = HashMap::new();
    let default = routes
        .iter()
        .find(|(p, _)| *p == "/")
        .map(|(_, b)| *b)
        .unwrap_or("");
    for (p, b) in routes {
        bodies.insert(*p, *b);
    }

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let req = String::from_utf8_lossy(&buf);
            // Parse the request line: "GET <path> HTTP/1.1"
            let path = req
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_string();
            let body = bodies.get(path.as_str()).copied().unwrap_or(default);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });

    format!("http://127.0.0.1:{port}")
}
