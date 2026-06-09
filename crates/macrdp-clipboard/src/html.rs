const HTML_PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
const HTML_SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";

/// Wrap HTML content in Windows "HTML Format" envelope with 10-digit byte offsets.
pub fn wrap_html_format(html: &str) -> Vec<u8> {
    let header_len = "Version:0.9\r\n\
                      StartHTML:0000000000\r\n\
                      EndHTML:0000000000\r\n\
                      StartFragment:0000000000\r\n\
                      EndFragment:0000000000\r\n"
        .len();

    let start_html = header_len;
    let start_fragment = header_len + HTML_PREFIX.len();
    let end_fragment = start_fragment + html.len();
    let end_html = end_fragment + HTML_SUFFIX.len();

    let header = format!(
        "Version:0.9\r\n\
         StartHTML:{start_html:010}\r\n\
         EndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\n\
         EndFragment:{end_fragment:010}\r\n"
    );

    let mut result = Vec::with_capacity(end_html);
    result.extend_from_slice(header.as_bytes());
    result.extend_from_slice(HTML_PREFIX.as_bytes());
    result.extend_from_slice(html.as_bytes());
    result.extend_from_slice(HTML_SUFFIX.as_bytes());
    result
}

/// Extract the HTML fragment from a Windows "HTML Format" payload.
pub fn unwrap_html_format(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    if !text.starts_with("Version:") {
        return None;
    }

    let mut start_fragment = None;
    let mut end_fragment = None;

    for line in text.lines() {
        if let Some(val) = line.strip_prefix("StartFragment:") {
            start_fragment = val.trim().parse::<usize>().ok();
        } else if let Some(val) = line.strip_prefix("EndFragment:") {
            end_fragment = val.trim().parse::<usize>().ok();
        }
    }

    let start = start_fragment?;
    let end = end_fragment?;
    if start > end || end > data.len() {
        return None;
    }

    Some(String::from_utf8_lossy(&data[start..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple() {
        let html = "<b>Hello</b>";
        let wrapped = wrap_html_format(html);
        let unwrapped = unwrap_html_format(&wrapped).unwrap();
        assert_eq!(unwrapped, html);
    }

    #[test]
    fn roundtrip_unicode() {
        let html = "<p>你好世界 🎉</p>";
        let wrapped = wrap_html_format(html);
        let unwrapped = unwrap_html_format(&wrapped).unwrap();
        assert_eq!(unwrapped, html);
    }

    #[test]
    fn roundtrip_empty() {
        let wrapped = wrap_html_format("");
        let unwrapped = unwrap_html_format(&wrapped).unwrap();
        assert_eq!(unwrapped, "");
    }

    #[test]
    fn header_has_version() {
        let wrapped = wrap_html_format("<b>test</b>");
        let header = std::str::from_utf8(&wrapped).unwrap();
        assert!(header.starts_with("Version:0.9\r\n"));
    }

    #[test]
    fn offsets_are_10_digits() {
        let wrapped = wrap_html_format("<b>test</b>");
        let text = std::str::from_utf8(&wrapped).unwrap();
        for line in text.lines() {
            if let Some(val) = line.strip_prefix("StartHTML:") {
                assert_eq!(val.len(), 10, "offset should be 10 digits, got: {val}");
            }
        }
    }

    #[test]
    fn unwrap_invalid_returns_none() {
        assert_eq!(unwrap_html_format(b"not html format"), None);
    }
}
