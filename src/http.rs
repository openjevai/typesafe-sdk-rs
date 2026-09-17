//! HTTP request preparation: URL joining, headers, body encoding, and timeouts.

use std::sync::LazyLock;
use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue, Method};
use reqwest::Url;
use serde_json::Value;

use crate::config::{Config, validate_timeout};
use crate::constants::{
    ACCEPT_HEADER, AUTHORIZATION_HEADER, CONTENT_TYPE_HEADER, JSON_CONTENT_TYPE,
    RETRY_COUNT_HEADER, RUNTIME_HEADER, SDK_HEADER, SDK_NAME, USER_AGENT_HEADER,
};
use crate::error::InvalidInputError;

/// `User-Agent` and `X-TypeSafe-SDK` value: `typesafe-sdk/{version}`.
static SDK_VERSION: LazyLock<String> = LazyLock::new(|| format!("{SDK_NAME}/{}", crate::VERSION));

/// `X-TypeSafe-Runtime` value: `rust/{version} ({os}; {arch})`.
static RUNTIME: LazyLock<String> = LazyLock::new(|| {
    format!(
        "rust/{} ({}; {})",
        env!("TYPESAFE_RUSTC_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
});

/// An HTTP request ready to be sent.
#[derive(Clone, Debug)]
pub(crate) struct PreparedRequest {
    /// HTTP method.
    pub(crate) method: Method,
    /// Absolute request URL.
    pub(crate) url: Url,
    /// Request headers, including the protected SDK headers.
    pub(crate) headers: HeaderMap,
    /// Encoded request body, or `None` for body-less requests.
    pub(crate) body: Option<Vec<u8>>,
    /// Timeout for this request, or `None` to leave it unset.
    pub(crate) timeout: Option<Duration>,
}

/// Builds a request from the client configuration, per-call headers, and an optional body.
///
/// Default headers are merged, then per-call headers; the retry-count header is stripped, and the
/// protected SDK headers always win. The body is encoded as JSON and sets `Content-Type`.
pub(crate) fn prepare(
    config: &Config,
    method: Method,
    path: &str,
    body: Option<Value>,
    timeout: Option<Duration>,
    extra_header_pairs: &[(String, String)],
    extra_headers: &HeaderMap,
) -> Result<PreparedRequest, InvalidInputError> {
    let mut headers = config.default_headers().clone();
    for (name, value) in extra_header_pairs {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| InvalidInputError::new(format!("invalid header name {name:?}")))?;
        let header_value = HeaderValue::from_str(value)
            .map_err(|_| InvalidInputError::new(format!("invalid header value for {name:?}")))?;
        headers.insert(header_name, header_value);
    }
    for (name, value) in extra_headers {
        headers.insert(name.clone(), value.clone());
    }
    headers.remove(RETRY_COUNT_HEADER);
    headers.insert(ACCEPT_HEADER, HeaderValue::from_static(JSON_CONTENT_TYPE));
    headers.insert(
        AUTHORIZATION_HEADER,
        HeaderValue::from_str(&format!("Bearer {}", config.api_key())).map_err(|_| {
            InvalidInputError::new(format!("invalid header value for {AUTHORIZATION_HEADER:?}"))
        })?,
    );
    headers.insert(
        USER_AGENT_HEADER,
        HeaderValue::from_str(&SDK_VERSION).expect("the SDK version is a valid header value"),
    );
    headers.insert(
        SDK_HEADER,
        HeaderValue::from_str(&SDK_VERSION).expect("the SDK version is a valid header value"),
    );
    headers.insert(
        RUNTIME_HEADER,
        HeaderValue::from_str(&RUNTIME).expect("the runtime string is a valid header value"),
    );

    let body = match body {
        Some(body) => {
            let encoded = serde_json::to_vec(&body).map_err(|error| {
                InvalidInputError::from_source(
                    "the request body could not be encoded as JSON",
                    error,
                )
            })?;
            headers.insert(
                CONTENT_TYPE_HEADER,
                HeaderValue::from_static(JSON_CONTENT_TYPE),
            );
            Some(encoded)
        }
        None => None,
    };

    let timeout = timeout.or(config.timeout());
    if let Some(timeout) = timeout {
        validate_timeout(timeout).map_err(InvalidInputError::new)?;
    }

    let url = Url::parse(&format!("{}{path}", config.base_url())).map_err(|_| {
        InvalidInputError::new(format!(
            "base_url must be an absolute http(s) URL: {}",
            config.base_url()
        ))
    })?;
    Ok(PreparedRequest {
        method,
        url,
        headers,
        body,
        timeout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigInput;
    use serde_json::json;

    fn config() -> Config {
        Config::resolve(
            ConfigInput {
                api_key: Some("test-key".to_owned()),
                base_url: Some("https://api.test/prefix///".to_owned()),
                model: Some("test-model".to_owned()),
                header_pairs: vec![("x-default".to_owned(), "kept".to_owned())],
                ..ConfigInput::default()
            },
            false,
        )
        .unwrap()
    }

    fn prepare_with(
        pairs: &[(String, String)],
        headers: &HeaderMap,
        body: Option<Value>,
    ) -> PreparedRequest {
        prepare(
            &config(),
            Method::POST,
            "/v1/systemone",
            body,
            None,
            pairs,
            headers,
        )
        .unwrap()
    }

    fn header<'a>(request: &'a PreparedRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .get(name)
            .map(|value| value.to_str().unwrap())
    }

    #[test]
    fn urls_join_the_configured_base_with_the_path() {
        let request = prepare_with(&[], &HeaderMap::new(), None);
        assert_eq!(request.url.as_str(), "https://api.test/prefix/v1/systemone");
        let request = prepare(
            &config(),
            Method::GET,
            "/v1/models",
            None,
            None,
            &[],
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(request.url.as_str(), "https://api.test/prefix/v1/models");
        assert_eq!(request.body, None);
    }

    #[test]
    fn protected_headers_win_over_user_values() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "custom-agent".parse().unwrap());
        headers.insert("x-typesafe-sdk", "custom-sdk".parse().unwrap());
        headers.insert("x-typesafe-runtime", "python/3.13".parse().unwrap());
        headers.insert("authorization", "Bearer impostor".parse().unwrap());
        headers.insert("accept", "text/plain".parse().unwrap());
        headers.insert("x-typesafe-retry-count", "9".parse().unwrap());
        headers.insert("x-call", "kept".parse().unwrap());
        let request = prepare_with(
            &[("Authorization".to_owned(), "Bearer other".to_owned())],
            &headers,
            None,
        );
        assert_eq!(header(&request, "authorization"), Some("Bearer test-key"));
        assert_eq!(header(&request, "accept"), Some("application/json"));
        assert_eq!(header(&request, "x-call"), Some("kept"));
        assert_eq!(header(&request, "x-default"), Some("kept"));
        assert_eq!(header(&request, "user-agent"), Some(SDK_VERSION.as_str()));
        assert_eq!(
            header(&request, "x-typesafe-sdk"),
            Some(SDK_VERSION.as_str())
        );
        assert_eq!(
            header(&request, "x-typesafe-runtime"),
            Some(RUNTIME.as_str())
        );
        assert!(header(&request, "x-typesafe-retry-count").is_none());
        assert!(RUNTIME.starts_with("rust/"), "{}", *RUNTIME);
    }

    #[test]
    fn content_type_follows_the_body() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/vnd.custom".parse().unwrap());
        let request = prepare_with(&[], &headers, Some(json!({"state": "x"})));
        assert_eq!(header(&request, "content-type"), Some("application/json"));
        assert_eq!(
            request.body.as_deref(),
            Some(br#"{"state":"x"}"#.as_slice())
        );
        let request = prepare_with(&[], &headers, None);
        assert_eq!(
            header(&request, "content-type"),
            Some("application/vnd.custom")
        );
        assert_eq!(request.body, None);
    }

    #[test]
    fn timeouts_prefer_the_per_call_value() {
        let config = config();
        let request = prepare(
            &config,
            Method::GET,
            "/v1/models",
            None,
            Some(Duration::from_secs(3)),
            &[],
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(request.timeout, Some(Duration::from_secs(3)));
        let request = prepare(
            &config,
            Method::GET,
            "/v1/models",
            None,
            None,
            &[],
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(request.timeout, Some(Duration::from_secs(10)));
        let config = Config::resolve(
            ConfigInput {
                api_key: Some("key".to_owned()),
                ..ConfigInput::default()
            },
            true,
        )
        .unwrap();
        let request = prepare(
            &config,
            Method::GET,
            "/v1/models",
            None,
            None,
            &[],
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(request.timeout, None);
    }

    #[test]
    fn invalid_timeouts_and_headers_are_rejected() {
        let error = prepare(
            &config(),
            Method::GET,
            "/v1/models",
            None,
            Some(Duration::ZERO),
            &[],
            &HeaderMap::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "timeout must be a positive, finite number of seconds."
        );
        let error = prepare(
            &config(),
            Method::GET,
            "/v1/models",
            None,
            None,
            &[("bad name".to_owned(), "v".to_owned())],
            &HeaderMap::new(),
        )
        .unwrap_err();
        assert_eq!(error.message(), "invalid header name \"bad name\"");
        let error = prepare(
            &config(),
            Method::GET,
            "/v1/models",
            None,
            None,
            &[("x-bad".to_owned(), "v\n".to_owned())],
            &HeaderMap::new(),
        )
        .unwrap_err();
        assert_eq!(error.message(), "invalid header value for \"x-bad\"");
    }

    #[test]
    fn extra_headers_override_earlier_pairs() {
        let mut headers = HeaderMap::new();
        headers.insert("x-call", "from-map".parse().unwrap());
        let request = prepare_with(
            &[("x-call".to_owned(), "from-pair".to_owned())],
            &headers,
            None,
        );
        assert_eq!(header(&request, "x-call"), Some("from-map"));
    }
}
