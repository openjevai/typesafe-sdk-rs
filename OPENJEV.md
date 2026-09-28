# OpenJEV Support

This fork of [typesafe-sdk-rs](https://github.com/netf/typesafe-sdk-rs) adds optional support for
[OpenJEV](https://openjev.sh) alongside the original TypeSafe API. TypeSafe remains the default;
anyone with a `TYPESAFE_API_KEY` sees zero behaviour change.

## What was added

| File | Change |
| --- | --- |
| `src/constants.rs` | `OPENJEV_API_KEY_ENV`, `JEV_PROVIDER_ENV`, `OPENJEV_DEFAULT_BASE_URL`, `OPENJEV_DEFAULT_MODEL` constants |
| `src/config.rs` | `Provider` enum, `resolve_provider` function, `provider` field on `ConfigInput` and `Config`, updated `Config::resolve` to select key env / default base URL / default model by provider |
| `src/client.rs` | `TypeSafeClient::provider()` accessor, `ClientBuilder::provider()` setter |
| `src/blocking.rs` | Same accessor and setter on the blocking client and its builder |
| `src/lib.rs` | Re-exports `Provider` |
| `tests/client_config.rs` | `EnvGuard` captures and removes `OPENJEV_API_KEY` and `JEV_PROVIDER` so the environment test stays hermetic |
| `README.md` | OpenJEV note after the intro; provider selection docs after the Configuration table |

## Provider selection rule

1. Explicit `.provider(Provider::OpenJEV)` on the builder wins.
2. `JEV_PROVIDER=openjev` (or `typesafe`) environment variable.
3. If an explicit `api_key` was passed to the builder → TypeSafe.
4. If `TYPESAFE_API_KEY` is set → TypeSafe (unchanged default).
5. If only `OPENJEV_API_KEY` is set → OpenJEV.
6. Otherwise → TypeSafe (key resolution fails with the TypeSafe env var name).

When OpenJEV is selected:
- API key comes from `OPENJEV_API_KEY` (or the explicit `api_key`).
- Default base URL is `https://api.openjev.sh`.
- Default model is `openjev`.
- `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL` still override the defaults.

## How to configure

```sh
# Automatic: set OPENJEV_API_KEY without TYPESAFE_API_KEY
export OPENJEV_API_KEY=oj-...

# Explicit:
export JEV_PROVIDER=openjev
export OPENJEV_API_KEY=oj-...
```

```rust
use typesafe_sdk::{Provider, TypeSafeClient};

let client = TypeSafeClient::builder()
    .api_key("oj-...")
    .provider(Provider::OpenJEV)
    .build()?;
```

## How it was verified

- A live POST to `https://api.openjev.sh/v1/systemone` with model `openjev`, state `ping`, and one
  noul question returned HTTP 200 (verified with the OpenJEV API key from the environment).
- Re-grepped the source to confirm no hardcoded `api.typesafe.ai` default was introduced: the
  existing default remains in `DEFAULT_BASE_URL`; the OpenJEV default lives in
  `OPENJEV_DEFAULT_BASE_URL` and is only used when the OpenJEV provider is selected.

## Upstream

Original project: https://github.com/netf/typesafe-sdk-rs by @netf (MIT license).
