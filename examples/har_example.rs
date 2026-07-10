//! HAR replay (`context.route_from_har`).
//!
//! Demonstrates serving a recorded response from a HAR file via the Fetch
//! domain. Because matching requests are fulfilled locally, the target URL
//! need NOT exist on the network — `http://har.test/hello` is a made-up host
//! that would normally fail to resolve, yet the navigation succeeds and
//! returns the canned body.
//!
//! Run: `cargo run --example har_example`

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    // --- 1. Build a minimal HAR 1.2 document with ONE entry. ---
    // The recorded URL is a fictional host; replay fulfills it offline.
    // The `mimeType` field name follows the HAR 1.2 spec (the parser renames
    // it internally to `mime_type`).
    const URL: &str = "http://har.test/hello";
    const BODY_MARKER: &str = "HAR-BODY-MARKER";
    const TITLE: &str = "HAR";

    let har = serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "har_example", "version": "0.1.0" },
            "entries": [
                {
                    "startedDateTime": "2024-01-01T00:00:00.000Z",
                    "time": 0,
                    "request": {
                        "method": "GET",
                        "url": URL,
                        "httpVersion": "HTTP/1.1",
                        "headers": [],
                        "queryString": [],
                        "headersSize": -1,
                        "bodySize": 0
                    },
                    "response": {
                        "status": 200,
                        "statusText": "OK",
                        "httpVersion": "HTTP/1.1",
                        "headers": [
                            { "name": "content-type", "value": "text/html" }
                        ],
                        "content": {
                            "size": 0,
                            "mimeType": "text/html",
                            "text": format!(
                                "<html><head><title>{TITLE}</title></head>\
                                 <body>{BODY_MARKER}</body></html>"
                            )
                        },
                        "redirectURL": "",
                        "headersSize": -1,
                        "bodySize": -1
                    },
                    "cache": {},
                    "timings": { "send": 0, "wait": 0, "receive": 0 }
                }
            ]
        }
    });

    let har_path = std::env::temp_dir().join("pwcdp_har_example.har");
    std::fs::write(&har_path, serde_json::to_vec_pretty(&har)?)?;
    println!("wrote HAR to {}", har_path.display());

    // --- 2. Launch Chrome and wire the context to replay from the HAR. ---
    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let context = browser.new_context().await?;
    // Register the route BEFORE navigating: the Fetch-domain interception must
    // be armed so the request is fulfilled from the HAR rather than the network.
    context.route_from_har(&har_path, None).await?;
    println!("HAR replay route registered for {URL}");

    // --- 3. Navigate to the recorded URL and read back the served page. ---
    let page = context.new_page().await?;
    page.goto(URL, None).await?;

    let html = page.content().await?;
    println!("page content:\n{html}");

    // The response body was served from the HAR, not the network.
    assert!(
        html.contains(BODY_MARKER),
        "page content should contain the HAR body marker {BODY_MARKER:?}"
    );

    let title: String = page.evaluate("document.title").await?;
    println!("document.title -> {title}");
    assert_eq!(title, TITLE, "title should come from the HAR-served document");

    println!("\nPASS");
    browser.close().await?;
    Ok(())
}
