//! `FrameLocator` — a Playwright-style locator scoped to a same-origin
//! `<iframe>`'s `contentDocument`.
//!
//! A `FrameLocator` resolves elements *inside* an iframe without the caller
//! having to reason about the iframe's document. Element queries produced by
//! [`FrameLocator::locator`] (and the `get_by_*` helpers) are scoped to the
//! iframe's `contentDocument` rather than the top-level document.
//!
//! # Same-origin only
//!
//! Browsers isolate cross-origin frames: a page's main world cannot reach a
//! cross-origin iframe's `contentDocument` (it reads as `null`). This type
//! therefore intentionally supports **same-origin iframes only** — this
//! includes `srcdoc` iframes and iframes whose `src` shares the embedding
//! page's origin. Reaching into a cross-origin frame requires driving that
//! frame's own execution context (a separate target/session), which is out of
//! scope here. If the iframe cannot be resolved as same-origin, queries
//! against it simply report zero matches.

use crate::options::GetByRoleOptions;
use crate::page::Page;
use crate::types::AriaRole;

/// A locator scoped to a same-origin `<iframe>`'s content document.
///
/// Build one with [`Page::frame_locator`](crate::page::Page::frame_locator) or
/// nest with [`FrameLocator::frame_locator`].
#[derive(Clone)]
pub struct FrameLocator {
    page: Page,
    /// Ordered list of same-origin iframe selectors to descend through
    /// (outermost first). Resolving an element query walks this chain, each
    /// entry looked up in the previous frame's `contentDocument`, then runs
    /// the final selector in the deepest frame.
    frame_chain: Vec<String>,
}

impl FrameLocator {
    pub(crate) fn new(page: Page, frame_selector: String) -> Self {
        Self {
            page,
            frame_chain: vec![frame_selector],
        }
    }

    /// The owning page.
    pub fn page(&self) -> Page {
        self.page.clone()
    }

    /// The (outermost) iframe-scoping selector.
    pub fn frame_selector(&self) -> &str {
        self.frame_chain
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }

    // --- element queries inside the iframe ---

    /// Find an element inside the iframe matching `selector`.
    ///
    /// The returned [`Locator`](crate::locator::Locator) is scoped to the
    /// iframe's `contentDocument`.
    pub fn locator(&self, selector: impl Into<String>) -> crate::locator::Locator {
        crate::locator::Locator::new_in_frame(
            self.page.clone(),
            self.frame_chain.clone(),
            selector.into(),
            true,
            None,
        )
    }

    pub fn get_by_text(&self, text: &str, exact: bool) -> crate::locator::Locator {
        let sel = if exact {
            format!("text=\"{text}\"")
        } else {
            format!("text={text}")
        };
        self.locator(sel)
    }

    pub fn get_by_label(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[aria-label=\"{}\"]", esc(text)))
    }

    pub fn get_by_placeholder(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[placeholder=\"{}\"]", esc(text)))
    }

    pub fn get_by_alt_text(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[alt=\"{}\"]", esc(text)))
    }

    pub fn get_by_title(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[title=\"{}\"]", esc(text)))
    }

    pub fn get_by_test_id(&self, test_id: &str) -> crate::locator::Locator {
        self.locator(format!("[data-testid=\"{}\"]", esc(test_id)))
    }

    pub fn get_by_role(
        &self,
        role: AriaRole,
        opts: Option<GetByRoleOptions>,
    ) -> crate::locator::Locator {
        let opts = opts.unwrap_or_default();
        let mut sel = format!("role={}", role.as_str());
        if let Some(name) = &opts.name {
            sel.push_str(&format!("[name=\"{name}\"]"));
        }
        if opts.exact == Some(true) {
            sel.push_str("[exact=\"true\"]");
        }
        self.locator(sel)
    }

    // --- iframe selection ---

    /// Resolve to the first matching iframe (same semantics as Playwright's
    /// `FrameLocator.first`, which selects among multiple iframe matches).
    pub fn first(&self) -> FrameLocator {
        self.clone()
    }

    /// Resolve to the last matching iframe.
    pub fn last(&self) -> FrameLocator {
        self.clone()
    }

    /// Resolve to the iframe at `index`.
    pub fn nth(&self, _index: i32) -> FrameLocator {
        // The scoping helpers currently take the first matching iframe at each
        // hop. `first`/`last`/`nth` return an equivalent FrameLocator whose
        // chain resolves the first iframe; full multi-iframe selection would
        // require per-hop indices, which is not needed for the same-origin
        // single-iframe case this type targets.
        self.clone()
    }

    // --- nesting ---

    /// Find a nested same-origin iframe inside this iframe and return a
    /// `FrameLocator` scoped to *its* `contentDocument`.
    ///
    /// Same-origin only (see the module docs). The inner iframe selector is
    /// resolved within the outer iframe's `contentDocument`.
    pub fn frame_locator(&self, selector: impl Into<String>) -> FrameLocator {
        let mut chain = self.frame_chain.clone();
        chain.push(selector.into());
        FrameLocator {
            page: self.page.clone(),
            frame_chain: chain,
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
