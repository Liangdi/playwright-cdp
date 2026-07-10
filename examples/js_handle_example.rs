//! `JSHandle` + `page.evaluate_handle`: hold a remote object and inspect it.
//!
//! Runs on a fresh page (no server / navigation needed). Demonstrates the
//! `JSHandle` lifecycle: materialize -> read by value -> fetch child props ->
//! enumerate props -> evaluate against the object -> dispose.
//!
//! Run: `cargo run --example js_handle_example`
//! Requires a Chrome/Chromium on the system, or set `CHROME_PATH`.

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::Playwright;
use serde_json::Value;

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

    // 1. Materialize a remote object via evaluate_handle -> JSHandle.
    let h = page
        .evaluate_handle("({a: 1, b: {c: 2}, list: [10,20,30]})")
        .await?;
    println!("(1) got JSHandle objectId={}", h.object_id());

    // 2. Read the whole object by value; assert v["a"] == 1.
    let v: Value = h.json_value().await?;
    println!("(2) json_value: {v}");
    assert_eq!(v["a"], 1, "v['a'] should be 1");

    // 3. Drill into a nested object: b -> c.
    let b = h.get_property("b").await?;
    let c = b.get_property_value("c").await?;
    println!("(3) b.c = {:?}", c);
    assert_eq!(c, Some(Value::from(2)), "b.c should be Some(2)");

    // 4. Enumerate own properties; assert the expected keys are present.
    let props = h.get_properties().await?;
    let mut keys: Vec<&String> = props.keys().collect();
    keys.sort();
    println!("(4) properties: {keys:?}");
    for expected in ["a", "b", "list"] {
        assert!(
            props.contains_key(expected),
            "properties should contain '{expected}'"
        );
    }

    // 5. Evaluate a page_function against the handle; assert list length == 3.
    //    The expression is wrapped as `(e, arg) => { return (<expr>); }` with
    //    the object bound as `e`, so reference it as `e` (an expression body,
    //    not a separate arrow function).
    let len: i64 = h.evaluate("e.list.length", None).await?;
    println!("(5) el.list.length = {len}");
    assert_eq!(len, 3, "list length should be 3");

    // 6. Release the remote object eagerly.
    h.dispose().await?;
    println!("(6) disposed handle");

    println!("PASS");

    browser.close().await?;
    Ok(())
}
