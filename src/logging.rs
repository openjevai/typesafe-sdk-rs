//! Credential redaction for log output.
//!
//! [`redact_headers`] and [`redact_pairs`] are the only header-formatting helpers this crate uses,
//! so no log call site can leak a credential.

use http::HeaderMap;

use crate::constants::SECRET_HEADERS;

/// Placeholder substituted for every redacted header value.
const REDACTED: &str = "***";

/// Returns whether a header value must be hidden from log output.
///
/// Matches [`SECRET_HEADERS`] case-insensitively, or any name containing `token` or `secret`.
pub(crate) fn is_secret_header(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    SECRET_HEADERS.contains(&lowered.as_str())
        || lowered.contains("token")
        || lowered.contains("secret")
}

/// Formats request or response headers as a single redacted line.
pub(crate) fn redact_headers(headers: &HeaderMap) -> String {
    let pairs = headers.iter().map(|(name, value)| {
        let value = value.to_str().unwrap_or("<non-utf8>");
        (name.as_str().to_owned(), value.to_owned())
    });
    redact_pairs(pairs)
}

/// Formats header name/value pairs as a single redacted line, hiding secret values.
pub(crate) fn redact_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> String {
    let rendered = pairs
        .into_iter()
        .map(|(name, value)| {
            format!(
                "{name}: {}",
                if is_secret_header(&name) {
                    REDACTED
                } else {
                    &value
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{rendered}}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_headers_are_detected() {
        for name in [
            "X-Access-Token",
            "x-MiXeD-ToKeN",
            "X-Client-Secret",
            "Cookie",
            "Set-Cookie",
            "Authorization",
            "PROXY-AUTHORIZATION",
            "x-api-key",
            "api-key",
        ] {
            assert!(is_secret_header(name), "{name} should be secret");
        }
    }

    #[test]
    fn non_secret_headers_are_preserved() {
        for name in ["Accept", "Content-Type", "X-TypeSafe-Retry-Count", "x-call"] {
            assert!(!is_secret_header(name), "{name} should not be secret");
        }
        let line = redact_pairs([
            ("Authorization".to_owned(), "Bearer sk".to_owned()),
            ("Accept".to_owned(), "application/json".to_owned()),
        ]);
        assert_eq!(line, "{Authorization: ***, Accept: application/json}");
    }

    #[test]
    fn header_maps_are_redacted() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret-key".parse().unwrap());
        headers.insert("x-typesafe-request-id", "req_01".parse().unwrap());
        let line = redact_headers(&headers);
        assert_eq!(line, "{authorization: ***, x-typesafe-request-id: req_01}");
    }
}
