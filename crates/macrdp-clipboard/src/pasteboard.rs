//! Safe Rust wrapper around macOS NSPasteboard (general pasteboard).
//!
//! objc2 0.6 API notes used here:
//! - `NSPasteboard::generalPasteboard()` returns `Retained<NSPasteboard>` (safe)
//! - `NSPasteboard::types()` returns `Option<Retained<NSArray<NSPasteboardType>>>`
//! - `NSPasteboard::clearContents()` returns `NSInteger` (new change_count)
//! - `NSPasteboard::stringForType()` returns `Option<Retained<NSString>>`
//! - `NSPasteboard::dataForType()` returns `Option<Retained<NSData>>`
//! - `NSPasteboard::setString_forType()` / `setData_forType()` return `bool`
//! - `NSData::with_bytes()` creates NSData from a &[u8]
//! - `NSData::to_vec()` copies bytes into a Vec<u8>

use std::path::PathBuf;

use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF};
use objc2_foundation::{NSData, NSString};
use tracing::{debug, warn};

/// Safe wrapper around `NSPasteboard.generalPasteboard`.
///
/// # Safety
///
/// `NSPasteboard` is not `Send` by default in objc2 because ObjC objects are
/// typically bound to a thread via autorelease pools. However, `PasteboardBridge`
/// is accessed exclusively from a single dedicated polling thread, so it is safe
/// to mark it as `Send`. The caller must ensure no concurrent access from
/// multiple threads.
pub struct PasteboardBridge;

// SAFETY: PasteboardBridge accesses NSPasteboard from a single dedicated
// polling thread only. No concurrent access from multiple threads occurs.
unsafe impl Send for PasteboardBridge {}

impl PasteboardBridge {
    /// Create a new `PasteboardBridge`.
    ///
    /// This does not allocate any ObjC objects; `NSPasteboard::generalPasteboard()`
    /// is called lazily on each method invocation, always returning the same
    /// system-wide singleton.
    pub fn new() -> Self {
        PasteboardBridge
    }

    /// Return the current pasteboard change count.
    ///
    /// Callers can poll this to detect clipboard changes without reading content.
    pub fn change_count(&self) -> i64 {
        let pb = NSPasteboard::generalPasteboard();
        pb.changeCount() as i64
    }

    /// Return all declared type UTI strings on the pasteboard.
    pub fn available_types(&self) -> Vec<String> {
        let pb = NSPasteboard::generalPasteboard();
        match pb.types() {
            None => Vec::new(),
            Some(arr) => {
                let count = arr.count();
                let mut result = Vec::with_capacity(count);
                for i in 0..count {
                    let ns_str = arr.objectAtIndex(i);
                    result.push(ns_str.to_string());
                }
                result
            }
        }
    }

    /// Read plain-text string from the pasteboard, if available.
    pub fn read_string(&self) -> Option<String> {
        let pb = NSPasteboard::generalPasteboard();
        // SAFETY: NSPasteboardTypeString is a valid static reference.
        let type_str: &NSString = unsafe { NSPasteboardTypeString };
        match pb.stringForType(type_str) {
            None => {
                debug!("pasteboard: no string data");
                None
            }
            Some(ns_str) => Some(ns_str.to_string()),
        }
    }

    /// Read image data from the pasteboard as PNG bytes.
    ///
    /// Tries PNG first, then falls back to TIFF (converting to PNG via the
    /// `image` crate).
    pub fn read_image(&self) -> Option<Vec<u8>> {
        let pb = NSPasteboard::generalPasteboard();

        // Try PNG first.
        let png_type: &NSString = unsafe { NSPasteboardTypePNG };
        if let Some(data) = pb.dataForType(png_type) {
            return Some(data.to_vec());
        }

        // Fallback: TIFF -> PNG via image crate.
        let tiff_type: &NSString = unsafe { NSPasteboardTypeTIFF };
        if let Some(data) = pb.dataForType(tiff_type) {
            let bytes = data.to_vec();
            match tiff_to_png(&bytes) {
                Ok(png) => return Some(png),
                Err(e) => warn!("pasteboard: failed to convert TIFF to PNG: {e}"),
            }
        }

        None
    }

    /// Read file URLs from the pasteboard.
    pub fn read_file_urls(&self) -> Vec<PathBuf> {
        let pb = NSPasteboard::generalPasteboard();

        // Use NSPasteboardTypeFileURL to check file URLs.
        // We rely on `readObjectsForClasses:options:` with NSURL class
        // but that API is unsafe and complex. Instead, iterate pasteboard
        // items and read string representation for NSPasteboardTypeFileURL.
        use objc2_app_kit::NSPasteboardTypeFileURL;
        let file_url_type: &NSString = unsafe { NSPasteboardTypeFileURL };

        let mut result = Vec::new();

        // Try reading as a single string (common case: one file copied).
        if let Some(url_str) = pb.stringForType(file_url_type) {
            let s = url_str.to_string();
            if let Some(path) = file_url_str_to_path(&s) {
                result.push(path);
            }
        }

        result
    }

    /// Write a plain-text string to the pasteboard.
    ///
    /// Returns the new change count, or -1 on failure.
    pub fn write_string(&self, text: &str) -> i64 {
        let pb = NSPasteboard::generalPasteboard();
        let change_count = pb.clearContents();

        let ns_str = NSString::from_str(text);
        let type_str: &NSString = unsafe { NSPasteboardTypeString };

        if !pb.setString_forType(&ns_str, type_str) {
            warn!("pasteboard: setString:forType: returned false");
            return -1;
        }

        change_count as i64
    }

    /// Write PNG image data to the pasteboard.
    ///
    /// Returns the new change count, or -1 on failure.
    pub fn write_image(&self, png_data: &[u8]) -> i64 {
        let pb = NSPasteboard::generalPasteboard();
        let change_count = pb.clearContents();

        let ns_data = NSData::with_bytes(png_data);
        let png_type: &NSString = unsafe { NSPasteboardTypePNG };

        if !pb.setData_forType(Some(&ns_data), png_type) {
            warn!("pasteboard: setData:forType: returned false");
            return -1;
        }

        change_count as i64
    }

    /// Clear the pasteboard contents.
    ///
    /// Returns the new change count.
    pub fn clear(&self) -> i64 {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents() as i64
    }
}

impl Default for PasteboardBridge {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `file://` URL string into a `PathBuf`.
fn file_url_str_to_path(url: &str) -> Option<PathBuf> {
    if let Some(path) = url.strip_prefix("file://") {
        // Percent-decode the path.
        let decoded = percent_decode(path);
        Some(PathBuf::from(decoded))
    } else {
        None
    }
}

/// Minimal percent-decoder for file paths (handles %XX escapes).
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Convert raw TIFF bytes to PNG bytes using the `image` crate.
fn tiff_to_png(tiff: &[u8]) -> anyhow::Result<Vec<u8>> {
    use image::ImageFormat;
    use std::io::Cursor;

    let img = image::load_from_memory_with_format(tiff, ImageFormat::Tiff)?;
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)?;
    Ok(out)
}
