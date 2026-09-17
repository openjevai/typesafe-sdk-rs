//! Configuration resolution from explicit options, environment variables, and defaults.

use std::fmt;
use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue};

use crate::constants::{
    API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL, DEFAULT_MODEL, DEFAULT_MODEL_ENV, DEFAULT_TIMEOUT,
};
use crate::error::ConfigError;
use crate::logging::redact_headers;

/// The settings a client builder resolves into a [`Config`].
#[derive(Clone, Debug, Default)]
pub(crate) struct ConfigInput {
    /// API key, taking precedence over `TYPESAFE_API_KEY`.
    pub(crate) api_key: Option<String>,
    /// API root, taking precedence over `TYPESAFE_BASE_URL`.
    pub(crate) base_url: Option<String>,
    /// Default model, taking precedence over `TYPESAFE_DEFAULT_MODEL`.
    pub(crate) model: Option<String>,
    /// Timeout applied to each HTTP operation.
    pub(crate) timeout: Option<Duration>,
    /// Timeout applied to connecting.
    pub(crate) connect_timeout: Option<Duration>,
    /// Headers set one name at a time, in the order they were set.
    pub(crate) header_pairs: Vec<(String, String)>,
    /// Headers set as a map, overriding the pairs.
    pub(crate) headers: Option<HeaderMap>,
}

/// Fully resolved client configuration.
#[derive(Clone)]
pub(crate) struct Config {
    /// API key sent as a bearer token.
    api_key: String,
    /// API root without trailing slashes.
    base_url: String,
    /// Model used when a call does not override it.
    default_model: String,
    /// Timeout applied to each HTTP operation, or `None` to leave it unset.
    timeout: Option<Duration>,
    /// Timeout applied to connecting, or `None` to leave it unset.
    connect_timeout: Option<Duration>,
    /// Headers applied to every request.
    default_headers: HeaderMap,
}

impl Config {
    /// Resolves the configuration, preferring explicit options and then non-empty environment values.
    ///
    /// `inherit_timeout` keeps an unset `timeout` unset, so a caller-supplied HTTP client's own
    /// default applies; otherwise the SDK default is used.
    pub(crate) fn resolve(input: ConfigInput, inherit_timeout: bool) -> Result<Self, ConfigError> {
        let api_key = resolve_value(input.api_key, API_KEY_ENV, None).ok_or_else(|| {
            ConfigError::new(format!("No API key was provided. Pass api_key or set the {API_KEY_ENV} environment variable."))
        })?;
        let base_url = resolve_value(input.base_url, BASE_URL_ENV, Some(DEFAULT_BASE_URL))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let default_model = resolve_value(input.model, DEFAULT_MODEL_ENV, Some(DEFAULT_MODEL))
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let base_url = normalize_base_url(&base_url)?;
        let timeout = match input.timeout {
            Some(timeout) => {
                validate_timeout(timeout).map_err(ConfigError::new)?;
                Some(timeout)
            }
            None if inherit_timeout => None,
            None => Some(DEFAULT_TIMEOUT),
        };
        if let Some(connect) = input.connect_timeout {
            validate_timeout(connect).map_err(ConfigError::new)?;
        }
        Ok(Self {
            api_key,
            base_url,
            default_model,
            timeout,
            connect_timeout: input.connect_timeout,
            default_headers: build_headers(&input.header_pairs, input.headers.as_ref())?,
        })
    }

    /// Returns the API root without trailing slashes.
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns the API key.
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Returns the model used when a call does not override it.
    pub(crate) fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Returns the timeout applied to each HTTP operation.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Returns the timeout applied to connecting.
    pub(crate) fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Returns the headers applied to every request.
    pub(crate) fn default_headers(&self) -> &HeaderMap {
        &self.default_headers
    }
}

impl fmt::Debug for Config {
    /// Renders the configuration without the API key or header values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("api_key", &"***")
            .field("base_url", &self.base_url)
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("default_headers", &redact_headers(&self.default_headers))
            .finish()
    }
}

/// Resolves one string setting: explicit value, then a non-empty environment value, then a default.
fn resolve_value(explicit: Option<String>, env: &str, default: Option<&str>) -> Option<String> {
    resolve_value_with(explicit, std::env::var(env).ok(), default)
}

/// [`resolve_value`] with the environment value already read, so the rules are testable directly.
fn resolve_value_with(
    explicit: Option<String>,
    environment: Option<String>,
    default: Option<&str>,
) -> Option<String> {
    match explicit {
        Some(value) => Some(value),
        None => environment
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| default.map(str::to_owned)),
    }
}

/// Validates a base URL and returns it with trailing slashes removed.
///
/// Parsing first means the stored value is the URL as the HTTP layer reads it, so anything the parser
/// normalizes — trailing whitespace, for instance — cannot fail later when a path is joined onto it.
fn normalize_base_url(base_url: &str) -> Result<String, ConfigError> {
    let invalid = || {
        ConfigError::new(format!(
            "base_url must be an absolute http(s) URL: {base_url}"
        ))
    };
    let url = reqwest::Url::parse(base_url).map_err(|_| invalid())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(invalid());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Rejects a zero timeout, the only invalid value a [`Duration`] can express.
pub(crate) fn validate_timeout(timeout: Duration) -> Result<(), String> {
    if timeout == Duration::ZERO {
        return Err("timeout must be a positive, finite number of seconds.".to_owned());
    }
    Ok(())
}

/// Builds a header map from name/value pairs and a header map, where a repeated name keeps its last value.
fn build_headers(
    pairs: &[(String, String)],
    headers: Option<&HeaderMap>,
) -> Result<HeaderMap, ConfigError> {
    let mut merged = HeaderMap::new();
    for (name, value) in pairs {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ConfigError::new(format!("invalid header name {name:?}")))?;
        let header_value = HeaderValue::from_str(value)
            .map_err(|_| ConfigError::new(format!("invalid header value for {name:?}")))?;
        merged.insert(header_name, header_value);
    }
    for (name, value) in headers.into_iter().flatten() {
        merged.insert(name.clone(), value.clone());
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings that pin the base URL and model, so only the environment can affect the API key.
    fn input(api_key: Option<String>, header_pairs: &[(String, String)]) -> ConfigInput {
        ConfigInput {
            api_key,
            base_url: Some("https://api.test".to_owned()),
            model: Some("test-model".to_owned()),
            header_pairs: header_pairs.to_vec(),
            ..ConfigInput::default()
        }
    }

    /// Resolves [`input`] with the given timeout inheritance.
    fn config(
        api_key: Option<String>,
        header_pairs: &[(String, String)],
        inherit_timeout: bool,
    ) -> Result<Config, ConfigError> {
        Config::resolve(input(api_key, header_pairs), inherit_timeout)
    }

    #[test]
    fn explicit_values_win_over_environment_values() {
        assert_eq!(
            resolve_value_with(
                Some("code-key".to_owned()),
                Some("env-key".to_owned()),
                Some("default")
            ),
            Some("code-key".to_owned())
        );
        assert_eq!(
            resolve_value_with(
                Some(String::new()),
                Some("env-key".to_owned()),
                Some("default")
            ),
            Some(String::new())
        );
    }

    #[test]
    fn environment_values_are_trimmed_and_blank_values_fall_back() {
        assert_eq!(
            resolve_value_with(None, Some("  env-key  ".to_owned()), None),
            Some("env-key".to_owned())
        );
        for blank in ["", " ", "\t\n "] {
            assert_eq!(
                resolve_value_with(None, Some(blank.to_owned()), Some("default")),
                Some("default".to_owned()),
                "{blank:?}"
            );
            assert_eq!(resolve_value_with(None, Some(blank.to_owned()), None), None);
        }
        assert_eq!(
            resolve_value_with(None, None, Some("default")),
            Some("default".to_owned())
        );
    }

    #[test]
    fn missing_keys_are_reported() {
        let error = config(None, &[], false).unwrap_err();
        assert!(
            error.message().contains("No API key was provided"),
            "{}",
            error.message()
        );
        assert!(
            error.message().contains("TYPESAFE_API_KEY"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn trailing_slashes_are_stripped_from_the_base_url() {
        let config = Config::resolve(
            ConfigInput {
                api_key: Some("key".to_owned()),
                base_url: Some("https://code.test///".to_owned()),
                model: Some("model".to_owned()),
                ..ConfigInput::default()
            },
            false,
        )
        .unwrap();
        assert_eq!(config.base_url(), "https://code.test");
    }

    #[test]
    fn base_urls_are_stored_as_the_http_layer_reads_them() {
        // Values the URL parser normalizes are stored normalized, so joining a path cannot fail later.
        for (input, expected) in [
            ("https://code.test ", "https://code.test"),
            ("https://code.test///", "https://code.test"),
            ("https://code.test/prefix///", "https://code.test/prefix"),
        ] {
            let resolved = Config::resolve(
                ConfigInput {
                    api_key: Some("key".to_owned()),
                    base_url: Some(input.to_owned()),
                    ..ConfigInput::default()
                },
                false,
            )
            .unwrap();
            assert_eq!(resolved.base_url(), expected, "{input:?}");
        }
    }

    #[test]
    fn defaults_apply_to_timeouts_and_models() {
        let default = config(Some("key".to_owned()), &[], false).unwrap();
        assert_eq!(default.timeout(), Some(Duration::from_secs(10)));
        assert_eq!(default.connect_timeout(), None);
        let inherited = config(Some("key".to_owned()), &[], true).unwrap();
        assert_eq!(inherited.timeout(), None);
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let base_url_error = |base_url: &str| {
            Config::resolve(
                ConfigInput {
                    api_key: Some("key".to_owned()),
                    base_url: Some(base_url.to_owned()),
                    ..ConfigInput::default()
                },
                false,
            )
            .unwrap_err()
            .message()
            .to_owned()
        };
        assert_eq!(
            base_url_error("/v1"),
            "base_url must be an absolute http(s) URL: /v1"
        );
        assert_eq!(
            base_url_error("ftp://example.test"),
            "base_url must be an absolute http(s) URL: ftp://example.test"
        );
        let error = Config::resolve(
            ConfigInput {
                api_key: Some("key".to_owned()),
                timeout: Some(Duration::ZERO),
                ..ConfigInput::default()
            },
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "timeout must be a positive, finite number of seconds."
        );
        let error = Config::resolve(
            ConfigInput {
                api_key: Some("key".to_owned()),
                connect_timeout: Some(Duration::ZERO),
                ..ConfigInput::default()
            },
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "timeout must be a positive, finite number of seconds."
        );
        let error =
            build_headers(&[("bad name".to_owned(), "value".to_owned())], None).unwrap_err();
        assert_eq!(error.message(), "invalid header name \"bad name\"");
        let error = build_headers(&[("x-bad".to_owned(), "value\n".to_owned())], None).unwrap_err();
        assert_eq!(error.message(), "invalid header value for \"x-bad\"");
    }

    #[test]
    fn headers_keep_their_last_value() {
        let headers = build_headers(
            &[
                ("x-call".to_owned(), "one".to_owned()),
                ("x-call".to_owned(), "two".to_owned()),
            ],
            None,
        )
        .unwrap();
        assert_eq!(headers.get("x-call").unwrap(), "two");
        assert_eq!(headers.len(), 1);
        let mut supplied = HeaderMap::new();
        supplied.insert("x-call", "from-map".parse().unwrap());
        let headers = build_headers(
            &[("x-call".to_owned(), "from-pair".to_owned())],
            Some(&supplied),
        )
        .unwrap();
        assert_eq!(headers.get("x-call").unwrap(), "from-map");
    }

    #[test]
    fn debug_hides_credentials() {
        let resolved = config(
            Some("private-api-key".to_owned()),
            &[
                ("x-token".to_owned(), "private-token".to_owned()),
                ("x-visible".to_owned(), "visible".to_owned()),
            ],
            false,
        )
        .unwrap();
        let rendered = format!("{resolved:?}");
        assert!(!rendered.contains("private-api-key"), "{rendered}");
        assert!(!rendered.contains("private-token"), "{rendered}");
        assert!(rendered.contains("visible"), "{rendered}");
    }
}
