//! Shared value types used across the public API.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Default action timeout in milliseconds (matches Playwright's default).
pub const DEFAULT_TIMEOUT_MS: f64 = 30_000.0;

/// A viewport size.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A point in viewport coordinates.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// Mouse button for click/hover actions.
///
/// Note: the discriminant values are an ordering, **not** the CDP `buttons`
/// bitmask. Use [`MouseButton::bitmask`] for the `Input.dispatchMouseEvent`
/// `buttons` field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum MouseButton {
    #[default]
    Left = 0,
    Middle = 1,
    Right = 2,
    Back = 3,
    Forward = 4,
}

impl MouseButton {
    pub fn as_str(&self) -> &'static str {
        match self {
            MouseButton::Left => "left",
            MouseButton::Middle => "middle",
            MouseButton::Right => "right",
            MouseButton::Back => "back",
            MouseButton::Forward => "forward",
        }
    }

    /// CDP `Input.dispatchMouseEvent` `buttons` bitmask for this button:
    /// Left=1, Right=2, Middle=4, Back=8, Forward=16.
    pub fn bitmask(&self) -> u8 {
        match self {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
            MouseButton::Back => 8,
            MouseButton::Forward => 16,
        }
    }
}

/// A rectangle in viewport coordinates (element bounds).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A clip rectangle for screenshots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScreenshotClip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Keyboard modifiers for actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardModifier {
    Alt,
    Control,
    ControlOrMeta,
    Meta,
    Shift,
}

/// Proxy server configuration.
#[derive(Debug, Clone, Default)]
pub struct ProxySettings {
    pub server: String,
    pub bypass: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// An ARIA role, used by `get_by_role`.
///
/// Mirrors Playwright's `AriaRole` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AriaRole {
    Alert,
    Alertdialog,
    Application,
    Article,
    Banner,
    Blockquote,
    Button,
    Caption,
    Cell,
    Checkbox,
    Code,
    Columnheader,
    Combobox,
    Complementary,
    Contentinfo,
    Definition,
    Deletion,
    Dialog,
    Directory,
    Document,
    Embed,
    Figure,
    Footer,
    Form,
    Generic,
    Grid,
    Gridcell,
    Group,
    Heading,
    Img,
    Image,
    Insertion,
    Link,
    List,
    Listbox,
    Listitem,
    Log,
    Main,
    Marquee,
    Math,
    Menu,
    Menubar,
    Menuitem,
    Menuitemcheckbox,
    Menuitemradio,
    Meter,
    Navigation,
    None,
    Note,
    Option,
    Paragraph,
    Presentation,
    Progressbar,
    Radio,
    Radiogroup,
    Region,
    Row,
    Rowgroup,
    Rowheader,
    Scrollbar,
    Search,
    Searchbox,
    Separator,
    Slider,
    Spinbutton,
    Status,
    Strong,
    Subscript,
    Superscript,
    Switch,
    Tab,
    Table,
    Tablist,
    Tabpanel,
    Term,
    Textbox,
    Time,
    Timer,
    Toolbar,
    Tooltip,
    Tree,
    Treegrid,
    Treeitem,
}

impl AriaRole {
    pub fn as_str(&self) -> &'static str {
        use AriaRole::*;
        match self {
            Alert => "alert",
            Alertdialog => "alertdialog",
            Application => "application",
            Article => "article",
            Banner => "banner",
            Blockquote => "blockquote",
            Button => "button",
            Caption => "caption",
            Cell => "cell",
            Checkbox => "checkbox",
            Code => "code",
            Columnheader => "columnheader",
            Combobox => "combobox",
            Complementary => "complementary",
            Contentinfo => "contentinfo",
            Definition => "definition",
            Deletion => "deletion",
            Dialog => "dialog",
            Directory => "directory",
            Document => "document",
            Embed => "embed",
            Figure => "figure",
            Footer => "footer",
            Form => "form",
            Generic => "generic",
            Grid => "grid",
            Gridcell => "gridcell",
            Group => "group",
            Heading => "heading",
            Img => "img",
            Image => "image",
            Insertion => "insertion",
            Link => "link",
            List => "list",
            Listbox => "listbox",
            Listitem => "listitem",
            Log => "log",
            Main => "main",
            Marquee => "marquee",
            Math => "math",
            Menu => "menu",
            Menubar => "menubar",
            Menuitem => "menuitem",
            Menuitemcheckbox => "menuitemcheckbox",
            Menuitemradio => "menuitemradio",
            Meter => "meter",
            Navigation => "navigation",
            None => "none",
            Note => "note",
            Option => "option",
            Paragraph => "paragraph",
            Presentation => "presentation",
            Progressbar => "progressbar",
            Radio => "radio",
            Radiogroup => "radiogroup",
            Region => "region",
            Row => "row",
            Rowgroup => "rowgroup",
            Rowheader => "rowheader",
            Scrollbar => "scrollbar",
            Search => "search",
            Searchbox => "searchbox",
            Separator => "separator",
            Slider => "slider",
            Spinbutton => "spinbutton",
            Status => "status",
            Strong => "strong",
            Subscript => "subscript",
            Superscript => "superscript",
            Switch => "switch",
            Tab => "tab",
            Table => "table",
            Tablist => "tablist",
            Tabpanel => "tabpanel",
            Term => "term",
            Textbox => "textbox",
            Time => "time",
            Timer => "timer",
            Toolbar => "toolbar",
            Tooltip => "tooltip",
            Tree => "tree",
            Treegrid => "treegrid",
            Treeitem => "treeitem",
        }
    }
}

/// Screenshot image format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScreenshotType {
    #[default]
    Png,
    Jpeg,
    Webp,
}

/// A cookie, shaped for `add_cookies` / `cookies`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Either `url` or `domain`+`path` must be supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// A name/value pair, e.g. a single localStorage entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameValue {
    pub name: String,
    pub value: String,
}

/// localStorage entries grouped by origin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct OriginStorage {
    pub origin: String,
    // Named to match the JS/Playwright `localStorage` key.
    pub localStorage: Vec<NameValue>,
}

/// A serializable snapshot of a context's storage: cookies plus per-origin
/// localStorage. Mirrors Playwright's `storageState` shape. Cookies stay as
/// `serde_json::Value` (the raw CDP/`Storage` cookie objects) so they can be
/// fed straight back to `set_storage_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageState {
    pub cookies: Vec<Value>,
    pub origins: Vec<OriginStorage>,
}

/// Extra HTTP headers map.
pub type Headers = HashMap<String, String>;

/// The source location of a [`ConsoleMessage`], mirroring the top frame of a
/// CDP `Runtime.consoleAPICalled` `stackTrace.callFrames[0]`.
///
/// All fields are optional since CDP omits them when no stack trace is
/// available (e.g. for some console methods or contexts without a script).
#[derive(Debug, Clone, Default)]
pub struct ConsoleMessageLocation {
    /// The script URL where the call originated.
    pub url: Option<String>,
    /// 1-based line number.
    pub line_number: Option<i64>,
    /// 1-based column number.
    pub column_number: Option<i64>,
}

impl ConsoleMessageLocation {
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
    pub fn line_number(&self) -> Option<i64> {
        self.line_number
    }
    pub fn column_number(&self) -> Option<i64> {
        self.column_number
    }
}

/// A console message captured from the page.
#[derive(Debug, Clone)]
pub struct ConsoleMessage {
    pub text: String,
    pub r#type: String,
    /// The source location, parsed from the call's `stackTrace.callFrames[0]`.
    /// `None` when CDP did not report a stack trace.
    pub location: Option<ConsoleMessageLocation>,
}

impl ConsoleMessage {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn r#type(&self) -> &str {
        &self.r#type
    }
    pub fn location(&self) -> Option<&ConsoleMessageLocation> {
        self.location.as_ref()
    }
}

/// An error thrown from page JavaScript (e.g. an unhandled exception or a
/// rejected promise observed via `Runtime.exceptionThrown`).
///
/// Self-contained DTO mirroring the useful fields of CDP's
/// `Runtime.exceptionThrown` payload.
#[derive(Debug, Clone, Default)]
pub struct WebError {
    /// Top-level message, if any.
    pub message: Option<String>,
    /// The stack trace, if any.
    pub stack: Option<String>,
    /// The script where it originated, if known.
    pub url: Option<String>,
    /// 1-based line/column, if known.
    pub line_number: Option<i64>,
    pub column_number: Option<i64>,
}

impl WebError {
    pub fn new() -> Self {
        Self::default()
    }
}

/// TLS/security details for a response, mirroring CDP's
/// `Security/securityStateChanged` / `Network.responseReceived` security info.
///
/// Self-contained DTO; all fields optional since CDP omits any that don't apply
/// to a given connection.
#[derive(Debug, Clone, Default)]
pub struct SecurityDetails {
    /// TLS protocol (e.g. `"TLS 1.3"`), if known.
    pub protocol: Option<String>,
    /// Cipher name, if known.
    pub cipher: Option<String>,
    /// Issuer of the server certificate, if known.
    pub issuer: Option<String>,
    /// Subject (CN) of the server certificate, if known.
    pub subject_name: Option<String>,
    /// Unix-epoch seconds at which the certificate is valid from, if known.
    pub valid_from: Option<f64>,
    /// Unix-epoch seconds at which the certificate expires, if known.
    pub valid_to: Option<f64>,
}

impl SecurityDetails {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a `SecurityDetails` from a CDP `SecurityDetails` JSON object (as
    /// carried by `Network.responseReceivedExtraInfo` /
    /// `Network.responseReceived`). Unknown shapes yield all-`None`.
    pub fn from_cdp(value: &Value) -> Self {
        let get = |k: &str| value.get(k).and_then(|v| v.as_str()).map(String::from);
        Self {
            protocol: get("protocol"),
            cipher: get("cipher"),
            issuer: get("issuer"),
            subject_name: get("subjectName").or_else(|| get("subject_name")),
            valid_from: value.get("validFrom").and_then(|v| v.as_f64()),
            valid_to: value.get("validTo").and_then(|v| v.as_f64()),
        }
    }
}
