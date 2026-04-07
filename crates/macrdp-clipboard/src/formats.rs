// Format conversion implemented in Task 6

/// Convert UTF-8 string to UTF-16LE bytes with null terminator.
pub fn utf8_to_utf16le(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    utf16.iter().flat_map(|&w| w.to_le_bytes()).collect()
}

/// Convert UTF-16LE bytes to UTF-8 string, stripping null terminator.
pub fn utf16le_to_utf8(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return Some(String::new());
    }
    let words: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&words)
        .ok()
        .map(|s| s.trim_end_matches('\0').to_string())
}

/// Map macOS UTI to RDP ClipboardFormat.
/// Returns a tuple of (format_id, optional_name) for now.
/// The exact ClipboardFormat construction depends on ironrdp-cliprdr API.
pub fn uti_to_rdp_format_id(uti: &str) -> Option<u32> {
    match uti {
        "public.utf8-plain-text" | "public.plain-text" => Some(13), // CF_UNICODETEXT
        "public.png" | "public.tiff" | "public.jpeg" => Some(8),    // CF_DIB
        "public.html" => Some(0), // registered "HTML Format" name
        "public.file-url" => Some(0), // registered "FileGroupDescriptorW" name
        _ => None,
    }
}

/// Map RDP format ID to macOS UTI.
pub fn rdp_format_id_to_uti(format_id: u32) -> Option<&'static str> {
    match format_id {
        13 => Some("public.utf8-plain-text"), // CF_UNICODETEXT
        8 => Some("public.png"),              // CF_DIB
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_to_utf16le_ascii() {
        let result = utf8_to_utf16le("hello");
        assert_eq!(result.len(), 12); // 5 chars + null = 6 code units = 12 bytes
        assert_eq!(result[0], 0x68); // 'h'
        assert_eq!(result[1], 0x00);
        assert_eq!(result[10], 0x00); // null terminator
        assert_eq!(result[11], 0x00);
    }

    #[test]
    fn utf8_to_utf16le_chinese() {
        let result = utf8_to_utf16le("你好");
        assert_eq!(result.len(), 6); // 2 chars + null = 3 code units = 6 bytes
        assert_eq!(result[0], 0x60); // '你' = U+4F60 → LE [0x60, 0x4F]
        assert_eq!(result[1], 0x4F);
    }

    #[test]
    fn utf16le_to_utf8_roundtrip() {
        let original = "Hello 世界! 🎉";
        let encoded = utf8_to_utf16le(original);
        let decoded = utf16le_to_utf8(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn utf16le_to_utf8_empty() {
        let result = utf16le_to_utf8(&[]);
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn uti_to_rdp_text() {
        assert_eq!(uti_to_rdp_format_id("public.utf8-plain-text"), Some(13));
    }

    #[test]
    fn uti_to_rdp_image() {
        assert_eq!(uti_to_rdp_format_id("public.png"), Some(8));
    }

    #[test]
    fn uti_to_rdp_unknown() {
        assert_eq!(uti_to_rdp_format_id("com.apple.custom"), None);
    }
}
