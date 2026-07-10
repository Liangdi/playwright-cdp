//! Rust <-> JS bindings (`expose_function`) + polling evaluation
//! (`wait_for_function`).
//!
//! Exposes a Rust `compute` function callable from the page, invokes it via
//! `evaluate` (which awaits the returned Promise), then uses `wait_for_function`
//! to poll a JS counter until it turns truthy.
//!
//! Run: `cargo run --example bindings`

use playwright_cdp::options::{LaunchOptions, WaitForFunctionOptions};
use playwright_cdp::Playwright;

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

    let page = browser.new_page().await?;
    page.set_content("<html><body></body></html>").await?;
    println!("page content set to an empty document");

    // --- expose_function: a Rust callback reachable from JS as `compute(...)`.
    //     It returns a JSON value; the JS wrapper wraps that in a Promise, so
    //     `evaluate` will await it. ---
    page.expose_function("compute", |args: Vec<serde_json::Value>| async move {
        let a = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!(a * b)
    })
    .await?;
    println!("exposed Rust `compute(a, b)` binding to the page");

    let r: i64 = page.evaluate("compute(6, 7)").await?;
    println!("compute(6, 7) = {r}");
    assert_eq!(r, 42);

    // --- wait_for_function: a JS counter that starts at 0 and increments; we
    //     poll `window.__n` until it becomes truthy (>= 1). ---
    // `evaluate` wraps the expression as `return (<expr>);`, so use a comma
    // sequence (no statement-level `;`) to set the counter and start the
    // interval in a single expression. setInterval returns an interval id
    // (an integer), so read it as i64 rather than unit.
    let _interval_id: i64 =
        page.evaluate("(window.__n = 0, setInterval(() => window.__n++, 20))")
            .await?;
    println!("started a JS interval incrementing window.__n");

    let n: i64 = page
        .wait_for_function(
            "window.__n",
            None,
            Some(WaitForFunctionOptions::new().timeout_ms(2000.0)),
        )
        .await?;
    println!("wait_for_function(window.__n) -> {n}");
    assert!(n >= 1, "counter should have advanced past 0");

    // --- evaluate_with_arg: feed a Rust value into the page expression. ---
    let squared: i64 = page
        .evaluate_with_arg("arg * arg", &9i64)
        .await?;
    println!("evaluate_with_arg(arg * arg, 9) = {squared}");
    assert_eq!(squared, 81);

    println!("\nall binding + polling steps completed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
