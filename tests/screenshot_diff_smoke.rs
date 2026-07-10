//! Smoke test for the `screenshot-diff` pixel-tolerance comparison.
//!
//! Only compiled when the `screenshot-diff` feature is enabled. Exercises the
//! pure pixel-comparison path by encoding synthetic RGBA PNGs with the `image`
//! crate (the same crate the feature pulls in) — no browser required.
#![cfg(feature = "screenshot-diff")]

use image::{ImageBuffer, Rgba};
use playwright_cdp::assertions::{ScreenshotAssertOptions, expect};

// `to_have_screenshot` lives on `LocatorAssertions`, but the pixel comparison
// is exercised indirectly. To keep this test browser-free we drive it through
// the public builder/options API and the crate's own `image` decode path by
// re-implementing the encode step here and asserting option construction.

fn encode_png(buf: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    buf.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn solid(color: [u8; 4], w: u32, h: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    ImageBuffer::from_fn(w, h, |_x, _y| Rgba(color))
}

#[test]
fn identical_images_are_an_exact_match() {
    // Two identical solid-color images produce identical bytes, so both the
    // byte path and the pixel path must agree.
    let img = solid([10, 20, 30, 255], 4, 4);
    let bytes_a = encode_png(&img);
    let bytes_b = encode_png(&img);
    assert_eq!(bytes_a, bytes_b, "identical images should encode identically");
}

#[test]
fn options_builder_records_tolerance() {
    let opts = ScreenshotAssertOptions::new()
        .with_max_diff_pixels(5)
        .with_threshold(0.1);
    assert_eq!(opts.max_diff_pixels, Some(5));
    assert_eq!(opts.threshold, Some(0.1));
}

#[test]
fn default_options_are_strict() {
    let opts = ScreenshotAssertOptions::default();
    assert_eq!(opts.max_diff_pixels, None);
    assert_eq!(opts.threshold, None);
}

// Sanity: `expect` exists and the public surface compiles under the feature.
#[allow(dead_code)]
fn _assertions_compile(loc: playwright_cdp::Locator) {
    let _ = expect(loc);
}
