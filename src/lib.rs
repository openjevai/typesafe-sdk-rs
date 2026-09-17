//! Rust SDK for the [TypeSafe AI](https://typesafe.ai) API, a community port of
//! [`typesafe-sdk-python`](https://github.com/typesafe-ai/typesafe-sdk-python).
//!
//! The crate documentation is the crate's README, so the quickstart, configuration table, and every
//! example are compiled as doctests.

#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations)]

mod client;
mod config;
mod decode;
mod error;
mod http;
mod json;
mod logging;
mod models;
mod question;
mod request;
mod response;
mod retry;

#[cfg(feature = "blocking")]
pub mod blocking;
pub mod constants;

pub use crate::client::{ClientBuilder, TypeSafeClient};
pub use crate::error::{
    ApiError, ConfigError, ConnectionError, Error, InvalidInputError, RateLimitError, Result,
    TimeoutError, ValidationError,
};
pub use crate::json::JsonContent;
pub use crate::models::{ListModelsRequest, Models};
pub use crate::question::{Choice, Noul, NoulCriteria, Question, Score};
pub use crate::request::SystemOneRequest;
pub use crate::response::{
    Answer, ChoiceAnswer, ListModelsResponse, ModelMetadata, NoulAnswer, RawResponse, ScoreAnswer,
    SystemOneResponse, UnknownAnswer, Usage,
};
pub use crate::retry::RetryPolicy;

pub use ::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};

/// Version of this SDK, reported in the `User-Agent` and `X-TypeSafe-SDK` headers.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
