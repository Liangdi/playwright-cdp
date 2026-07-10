# playwright-cdp

**[中文](README.md)** | [English](README.en.md)

> 直接通过 **Chrome DevTools Protocol (CDP)** 驱动 Chromium —— 一套 Playwright 风格的 Rust API,**无需 Node.js driver**。

[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange)](https://www.rust-lang.org/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`playwright-cdp` 通过单条 WebSocket 原生地讲 CDP 协议。它的 API 形状镜像 Playwright
(`Playwright` → `BrowserType` → `Browser` → `BrowserContext` → `Page` → `Locator`,
builder 选项结构体、`async`/`Result<T>`),但**不**去 spawn Playwright 的 Node driver 再
代理转发,而是直接拉起一个 Chromium 进程并原生驱动。

## 为什么做这个

[`playwright-rust`](https://github.com/nickel-org/playwright-rust)(以及上游 Playwright)
会 shell 出一个 Node.js driver 二进制,通过自定义管道协议通信。能用,但代价是:

- 强依赖 Node.js 运行时 + Playwright 的 `node_modules`;
- 多一个 IPC 跳板和需要管理的子进程;
- Rust crate 与已安装 driver 之间的版本耦合。

`playwright-cdp` 把这些都去掉了。系统里只要有 Chrome/Chromium 可执行文件,剩下全是纯
Rust 直接讲 CDP。

## 状态

当前为早期 `0.1.x`。核心 API 已实现,并对真实 Chrome 做了端到端验证(`cargo test`
全绿,见[验证](#验证))。尚未覆盖 Playwright 的全部特性 —— 见
[已实现](#已实现)与[限制](#限制)。

## 快速开始

```toml
[dependencies]
playwright-cdp = "0.1"
tokio = { version = "1", features = ["full"] }
```

```rust,no_run
use playwright_cdp::{Playwright, options::LaunchOptions};

#[tokio::main]
async fn main() -> playwright_cdp::Result<()> {
    // 三层入口,镜像 playwright-rust(此处不 spawn 任何 Node driver)。
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

如果你不需要 `Playwright`/`BrowserType` 这一层,直接 `Browser::launch(opts)` 也可以。

### 定位器与交互

```rust,no_run
# use playwright_cdp::types::AriaRole;
# // page: playwright_cdp::Page
# # let page: playwright_cdp::Page = unimplemented!();
page.locator("#username").fill("alice", None).await?;
page.get_by_role(AriaRole::Button, None).click(None).await?;
let count: usize = page.locator("li").count().await?;
# playwright_cdp::Result::<()>::Ok(())
```

### 网络拦截

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

## 已实现

以下全部是基于 CDP 的**真实实现**(非桩函数):

| 领域 | 类型 / 能力 |
| --- | --- |
| **入口** | `Playwright`、`BrowserType`(`chromium`/`firefox`/`webkit`)、`Browser::launch` / `connect_over_cdp` |
| **上下文** | `BrowserContext` —— 页面、cookies、额外 headers、`add_init_script`、`set_default_timeout` / `set_default_navigation_timeout`、`grant_permissions` / `clear_permissions`、`storage_state`(cookies **+ localStorage**)/ `set_storage_state`、上下文级 `route` |
| **页面 / 帧** | `Page`、`Frame`、`FrameLocator` —— `goto`/`reload`/`go_back`/`go_forward`/`wait_for_url`/`wait_for_load_state`、`evaluate`/`eval_on_selector`/`wait_for_function`、`query_selector(_all)`、`screenshot`、`pdf`、`set_viewport_size`、`emulate_media`、`set_offline`、`set_cache_disabled`、`bring_to_front`、`set_geolocation`、`drag_and_drop`、`expose_function` |
| **定位器** | `Locator` —— click/dblclick/fill/clear/check/uncheck/set_checked/press/hover/tap/drag_to/select_option/set_input_files/dispatch_event/scroll_into_view_if_needed,文本/属性/状态查询,`nth`/`first`/`last`/`all`/`filter`/`and_`/`or_`、`evaluate(_all)`、`bounding_box`、`screenshot`、`wait_for`、`aria_snapshot` |
| **元素与输入** | `ElementHandle`、`Keyboard`、`Mouse`、`Touchscreen` |
| **网络** | `Request`、`Response`(status/headers/body/text/json)、`Route`(`continue_`/`fulfill`/`abort`/`fallback`)、`on_request`/`on_response`/`on_requestfinished`/`on_requestfailed`、基于在途请求计数的 `networkidle` |
| **事件** | `on_console`、`on_dialog`(+ `Dialog`)、`on_pageerror`、`on_download`(+ `Download`)、`on_filechooser`(+ `FileChooser`)、`on_worker`(+ `Worker`)、`on_page`、`on_close` |
| **诊断** | `Tracing`(经 Tracing 域采集 Chrome trace)、`aria_snapshot`(无障碍树 → Playwright YAML)、`ConsoleMessage` |
| **API 测试** | `APIRequestContext` / `APIResponse` —— 基于 `reqwest` 的独立 HTTP 客户端(`page.request()`、`context.request()`) |
| **断言** | `expect(locator)` / `expect_page(page)` —— `to_be_visible/hidden/enabled/disabled/editable/checked`、`to_have_text/contain_text/inner_text`、`to_have_count/attribute/value`、`to_have_title/url`,支持 `.not()` 与 `.with_timeout(...)` |

选择器引擎(`text=`、`xpath=`、`role=`、CSS、`>>` 链式)是一个按执行上下文注入的小脚本;
同源 `<iframe>` 遍历可通过 `FrameLocator` 完成。

## 示例

[`examples/`](examples/) 下有十个可运行示例,每个都是针对真实浏览器的端到端演示:

| 示例 | 演示内容 |
| --- | --- |
| [`demo`](examples/demo.rs) | 启动、导航、求值、定位器点击、fill、截图 |
| [`interactions`](examples/interactions.rs) | click/fill/check/select/dblclick/hover/press + keyboard/mouse/touchscreen |
| [`assertions`](examples/assertions.rs) | `expect` / `expect_page` 断言 |
| [`network`](examples/network.rs) | 请求/响应捕获、`route` fulfill/abort |
| [`api_request`](examples/api_request.rs) | 独立的 `page.request()` HTTP 客户端 |
| [`frames`](examples/frames.rs) | `FrameLocator` + 帧树 |
| [`page_capture`](examples/page_capture.rs) | 截图、PDF、`aria_snapshot` |
| [`dialogs_downloads`](examples/dialogs_downloads.rs) | `on_dialog`、`on_download` |
| [`contexts`](examples/contexts.rs) | 上下文隔离、cookies、`storage_state` 往返 |
| [`bindings`](examples/bindings.rs) | `expose_function`、`wait_for_function` |

```sh
cargo run --example demo
cargo run --example network
cargo run --example contexts
```

## 验证

```sh
cargo build --all-targets   # 零 warning
cargo test                  # 50+ 集成 + 单元测试(找不到 Chrome 时自动跳过)
cargo run --example demo
```

需要浏览器的测试在找不到 Chrome/Chromium 时会优雅跳过(可用 `CHROME_PATH`
或在 `LaunchOptions` 里指定 `executable_path` 来固定一个)。

## 工作原理

```
你的应用 ──► playwright-cdp (Rust)
                 │  拉起并拥有 Chrome 进程(--remote-debugging-port)
                 │  打开一条 ws:// 连接(flatten,多路复用)
                 └──► Chrome DevTools Protocol(Target / Page / Runtime / Network
                      / DOM / Input / Fetch / Emulation / Accessibility / Tracing …)
```

- `BrowserProcess` 发现 Chrome 可执行文件(`executable_path`、`CHROME_PATH`、
  常见安装路径、`channel`),以 `--remote-debugging-port=0` 启动,读取
  `DevToolsActivePort`,解析出浏览器 WebSocket 端点。
- `CdpConnection` 在单条 socket 上多路复用请求/响应配对与事件分发;flatten 的
  auto-attach 让弹窗/worker 复用同一条连接。
- 页面级求值走默认(主世界)的 `Runtime.evaluate`,这样导航后不会因缓存的
  执行上下文 id 失效而出错。

## 限制

- **仅支持 Chromium。** Firefox/WebKit 返回的是一个 Chromium 后端的 `BrowserType`
  并打印警告(CDP 是 Chromium 专有协议)。
- **`FrameLocator` 仅支持同源**(跨源 iframe 无法从父框架主世界访问)。
- 对 headless Chrome 150 的怪行为做了防御式处理:例如 `FileChooser` 走
  `DOM.setFileInputFiles`(该版本的 `Page.handleFileChooser` 不被支持);
  `Download` 在收不到 `Completed` 进度事件时回退到文件系统轮询。
- 尚未实现:`Selectors.register`(自定义选择器引擎)、popup-as-`Page`
  (`opener`/`on_popup`)、`recordVideo`、旧的 `page.accessibility.snapshot()`
  JSON API。`aria_snapshot` 力求贴近 Playwright 格式,但不保证边缘情况字节级一致。

## 许可证

Apache-2.0,见 [LICENSE](LICENSE)。
