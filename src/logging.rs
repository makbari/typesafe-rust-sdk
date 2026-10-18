use reqwest::header::HeaderMap;

/// Mask a credential, keeping its scheme and the last four characters of secrets longer
/// than eight.
fn redact_key(value: &str) -> String {
    let (scheme, secret) = match value.split_once(char::is_whitespace) {
        Some((scheme, rest)) => (Some(scheme), rest.trim()),
        None => (None, value),
    };
    let tail = if secret.chars().count() > 8 {
        &secret[secret.char_indices().rev().nth(3).map_or(0, |(i, _)| i)..]
    } else {
        ""
    };
    format!(
        "{}***{tail}",
        scheme.map_or(String::new(), |s| format!("{s} "))
    )
}

/// Headers as `name: value` pairs for logs, with known credentials masked.
pub(crate) fn redacted_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let raw = value.to_str().unwrap_or("<binary>");
            let shown = match name.as_str() {
                "authorization" | "proxy-authorization" | "x-api-key" => redact_key(raw),
                "cookie" | "set-cookie" => "***".to_owned(),
                _ => raw.to_owned(),
            };
            format!("{name}: {shown}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    #[test]
    fn masks_credentials_only() {
        assert_eq!(redact_key("Bearer sk-abcdefgh1234"), "Bearer ***1234");
        assert_eq!(redact_key("Bearer short"), "Bearer ***");
        assert_eq!(redact_key("keykeykeykey"), "***ykey");
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_static("Bearer sk-abcdefgh1234"),
        );
        h.insert("cookie", HeaderValue::from_static("session=top-secret"));
        h.insert("accept", HeaderValue::from_static("application/json"));
        let shown = redacted_headers(&h);
        assert!(!shown.contains("abcdefgh") && !shown.contains("top-secret"));
        assert!(shown.contains("accept: application/json"));
    }
}
