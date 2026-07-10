# playwright-cdp

[中文](README.md) | **[English](README.en.md)**

> Drive Chromium directly over the **Chrome DevTools Protocol (CDP)** — a Playwright-shaped Rust API with **no Node.js driver** required.

[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange)](https://www.rust-lang.org/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`playwright-cdp` speaks CDP natively over a single WebSocket. It mirrors the
Playwright API shape (`Playwright` → `BrowserType` → `Browser` → `BrowserContext`
→ `Page` → `Locator`, builder option structs, `async`/`Result<T>`), but instead
of spawning the Playwright Node driver and proxying through it, it launches a
 Chromium process and drives it directly.

## Why

[`playwright-rust`](https://github.com/nickel-org/playwright-rust) (and upstream
Playwright) shells out to a Node.js driver binary and communicates over a custom
pipe protocol. That works, but it means:

- a Node.js runtime + Playwright's `node_modules` are a hard dependency,
- an extra IPC hop and process to manage,
- version coupling between the Rust crate and the installed driver.

`playwright-cdp` removes all of that. You only need a Chrome/Chromium binary on
the system — everything else is pure Rust talking CDP.

## Status

This is an early `0.1.x`. The core API surface is implemented and verified
end-to-end against real Chrome (`cargo test` is green; see
[Verification](#verification)). Not every Playwright feature is covered yet — see
[What's implemented](#whats-implemented) and [Limitations](#limitations).

## Quick start

```toml
[dependencies]
playwright-cdp = "0.1"
tokio = { version = "1", features = ["full"] }
```

```rust,no_run
use playwright_cdp::{Playwright, options::LaunchOptions};

#[tokio::main]
async fn main() -> playwright_cdp::Result<()> {
    // Three-layer entry, mirroring playwright-rust (no Node driver spawned here).
    let browser = Playwright::launch().await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;

    let page = browser.new_page().await?;
    page.goto("https://example.com", None).await?;
    let title: String = page.evaluate("document.title").await?;
    println!("{title}");

    browser.close().await?;
    Ok(())
}
```

`Browser::launch(opts)` also works directly if you don't need the
`Playwright`/`BrowserType` layer.

### Locators & interactions

```rust,no_run
# use playwright_cdp::types::AriaRole;
# // page: playwright_cdp::Page
# # let page: playwright_cdp::Page = unimplemented!();
page.locator("#username").fill("alice", None).await?;
page.get_by_role(AriaRole::Button, None).click(None).await?;
let count: usize = page.locator("li").count().await?;
# playwright_cdp::Result::<()>::Ok(())
```

### Network interception

```rust,no_run
# use playwright_cdp::route::RouteFulfillOptions;
# # let page: playwright_cdp::Page = unimplemented!();
page.route("**/api/user", |route| async move {
    route.fulfill(
        RouteFulfillOptions::new()
            .status(200)
            .content_type("application/json")
            .body(r#"{"name":"alice"}"#),
    ).await.ok();
}).await?;
# playwright_cdp::Result::<()>::Ok(())
```

## What's implemented

All of the following are real implementations backed by CDP (not stubs):

| Area | Types / features |
| --- | --- |
| **Entry** | `Playwright`, `BrowserType` (`chromium`/`firefox`/`webkit`), `Browser::launch` / `connect_over_cdp` |
| **Contexts** | `BrowserContext` — pages, cookies, extra headers, `add_init_script`, `set_default_timeout` / `set_default_navigation_timeout`, `grant_permissions` / `clear_permissions`, `storage_state` (cookies **+ localStorage**) / `set_storage_state`, context-level `route` |
| **Pages / Frames** | `Page`, `Frame`, `FrameLocator` — `goto`/`reload`/`go_back`/`go_forward`/`wait_for_url`/`wait_for_load_state`, `evaluate`/`eval_on_selector`/`wait_for_function`, `query_selector(_all)`, `screenshot`, `pdf`, `set_viewport_size`, `emulate_media`, `set_offline`, `set_cache_disabled`, `bring_to_front`, `set_geolocation`, `drag_and_drop`, `expose_function` |
| **Locators** | `Locator` — click/dblclick/fill/clear/check/uncheck/set_checked/press/hover/tap/drag_to/select_option/set_input_files/dispatch_event/scroll_into_view_if_needed, text/attribute/state queries, `nth`/`first`/`last`/`all`/`filter`/`and_`/`or_`, `evaluate(_all)`, `bounding_box`, `screenshot`, `wait_for`, `aria_snapshot` |
| **Elements & input** | `ElementHandle`, `Keyboard`, `Mouse`, `Touchscreen` |
| **Network** | `Request`, `Response` (status/headers/body/text/json), `Route` (`continue_`/`fulfill`/`abort`/`fallback`), `on_request`/`on_response`/`on_requestfinished`/`on_requestfailed`, `networkidle` via in-flight request counting |
| **Events** | `on_console`, `on_dialog` (+ `Dialog`), `on_pageerror`, `on_download` (+ `Download`), `on_filechooser` (+ `FileChooser`), `on_worker` (+ `Worker`), `on_page`, `on_close` |
| **Diagnostics** | `Tracing` (Chrome trace via `Tracing` domain), `aria_snapshot` (accessibility tree → Playwright YAML), `ConsoleMessage` |
| **API testing** | `APIRequestContext` / `APIResponse` — standalone HTTP client (`page.request()`, `context.request()`) via `reqwest` |
| **Assertions** | `expect(locator)` / `expect_page(page)` — `to_be_visible/hidden/enabled/disabled/editable/checked`, `to_have_text/contain_text/inner_text`, `to_have_count/attribute/value`, `to_have_title/url`, with `.not()` and `.with_timeout(...)` |

The selector engine (`text=`, `xpath=`, `role=`, CSS, `>>` chaining) is a small
injected script scoped per execution context; same-origin `<iframe>` traversal
works via `FrameLocator`.

## Examples

Ten runnable examples live in [`examples/`](examples/) — each is an end-to-end
demo against a real browser:

| Example | Demonstrates |
| --- | --- |
| [`demo`](examples/demo.rs) | launch, navigate, evaluate, locator click, fill, screenshot |
| [`interactions`](examples/interactions.rs) | click/fill/check/select/dblclick/hover/press + keyboard/mouse/touchscreen |
| [`assertions`](examples/assertions.rs) | `expect` / `expect_page` assertions |
| [`network`](examples/network.rs) | request/response capture, `route` fulfill/abort |
| [`api_request`](examples/api_request.rs) | standalone `page.request()` HTTP client |
| [`frames`](examples/frames.rs) | `FrameLocator` + the frame tree |
| [`page_capture`](examples/page_capture.rs) | screenshots, PDF, `aria_snapshot` |
| [`dialogs_downloads`](examples/dialogs_downloads.rs) | `on_dialog`, `on_download` |
| [`contexts`](examples/contexts.rs) | context isolation, cookies, `storage_state` round-trip |
| [`bindings`](examples/bindings.rs) | `expose_function`, `wait_for_function` |

```sh
cargo run --example demo
cargo run --example network
cargo run --example contexts
```

## Verification

```sh
cargo build --all-targets   # warning-free
cargo test                  # 50+ integration + unit tests (skips automatically if no Chrome is found)
cargo run --example demo
```

Tests that need a browser skip gracefully when no Chrome/Chromium is discoverable
(set `CHROME_PATH` or `executable_path` in `LaunchOptions` to pin one).

## How it works

```
your app ──► playwright-cdp (Rust)
                 │  spawns + owns the Chrome process (--remote-debugging-port)
                 │  opens one ws:// connection (flattened, multiplexed)
                 └──► Chrome DevTools Protocol (Target / Page / Runtime / Network
                      / DOM / Input / Fetch / Emulation / Accessibility / Tracing …)
```

- `BrowserProcess` discovers the Chrome binary (`executable_path`, `CHROME_PATH`,
  known install locations, `channel`), launches it with `--remote-debugging-port=0`,
  reads `DevToolsActivePort`, and resolves the browser WebSocket endpoint.
- `CdpConnection` multiplexes request/response correlation and event fan-out over
  the single socket; flattened auto-attach keeps popups/workers on the same connection.
- Page-level evaluation uses `Runtime.evaluate` in the default (main) world, which
  survives navigations without caching a fragile execution-context id.

## Limitations

- **Chromium only.** Firefox/WebKit return a Chromium-backed `BrowserType` with a
  warning (CDP is a Chromium protocol).
- **`FrameLocator` is same-origin only** (cross-origin iframes can't be reached
  from the parent's main world).
- Headless Chrome 150 quirks are handled defensively, e.g. `FileChooser` uses
  `DOM.setFileInputFiles` (`Page.handleFileChooser` isn't supported on this build),
  and `Download` falls back to a filesystem watch when the `Completed` progress
  event isn't emitted.
- Not yet implemented: `Selectors.register` (custom selector engines),
  popup-as-`Page` (`opener`/`on_popup`), `recordVideo`, the legacy
  `page.accessibility.snapshot()` JSON API. `aria_snapshot` aims for Playwright's
  format but isn't guaranteed byte-identical in edge cases.

## License

Apache-2.0. See [LICENSE](LICENSE).
