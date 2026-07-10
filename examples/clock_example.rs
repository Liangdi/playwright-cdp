//! Fake timers (`page.clock()`).
//!
//! Demonstrates the Playwright-style fake-timer API:
//! - `clock.set_fixed_time` pins `Date.now()` so it stops advancing;
//! - `clock.advance(ms)` moves the virtual clock forward, firing every
//!   fake timer whose deadline falls due along the way.
//!
//! Run: `cargo run --example clock_example`

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

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    page.set_content("<html><body></body></html>").await?;
    println!("page content set to an empty document");

    let clock = page.clock();

    // --- 1. set_fixed_time: pin Date.now() to a fixed epoch-ms value. ---
    // After this, both Date.now() and new Date() read the pinned time and do
    // NOT advance with real time.
    const FIXED: i64 = 1_700_000_000_000; // 2023-11-14T22:13:20Z
    clock.set_fixed_time(Some(FIXED)).await?;
    let now: f64 = page.evaluate("Date.now()").await?;
    println!("set_fixed_time({FIXED}) -> Date.now() = {now}");
    assert!(
        (now - FIXED as f64).abs() < 1.0,
        "fixed clock should read exactly {FIXED}, got {now}"
    );

    // --- 2. advance: move the virtual clock forward by 5000 ms. ---
    // advance() un-pins the clock and jumps it forward, draining timers due
    // along the way. Date.now() should now be ~ FIXED + 5000.
    clock.advance(5_000).await?;
    let after: f64 = page.evaluate("Date.now()").await?;
    println!("advance(5000) -> Date.now() = {after}");
    let expected = (FIXED + 5_000) as f64;
    assert!(
        (after - expected).abs() < 100.0,
        "clock should have advanced by ~5000 to {expected}, got {after}"
    );
    println!(
        "delta from fixed base: {:.0} ms (expected ~5000)",
        after - FIXED as f64
    );

    // --- 3. Fake setTimeout actually fires when the clock advances. ---
    // The clock controller overrides `setTimeout`, but ONLY for timers
    // scheduled *after* `install()`/`advance()` engaged the fakes. So we
    // schedule the timer here (post-install) via `evaluate`, then jump the
    // clock past its 1000 ms deadline. The pending callback runs inside
    // `advance()` and sets `window.__fired = true`.
    //
    // (Scheduling the timer before installing the clock would NOT be faked:
    // it would land on the native scheduler, which `advance()` cannot drain.)
    let scheduled: i64 = page
        .evaluate("setTimeout(() => { window.__fired = true; }, 1000)")
        .await?;
    println!("scheduled fake setTimeout (id={scheduled}, 1000 ms)");

    // Not fired yet: the deadline (1000 ms ahead) hasn't been reached.
    let fired_before: bool = page.evaluate("!!window.__fired").await?;
    println!("window.__fired before advance -> {fired_before}");
    assert!(!fired_before, "timer should not have fired before advancing");

    // Jump 2000 ms past the current virtual time — well beyond the 1000 ms
    // deadline, so the pending timer fires during the drain.
    clock.advance(2_000).await?;
    let fired_after: bool = page.evaluate("!!window.__fired").await?;
    println!("window.__fired after advance(2000) -> {fired_after}");
    assert!(
        fired_after,
        "fake setTimeout(1000) should have fired after advancing 2000 ms"
    );

    println!("\nPASS");
    browser.close().await?;
    Ok(())
}
