# typesafe-rust-sdk

Rust port of the TypeScript SDK https://github.com/typesafe-ai/typesafe-sdk-js (crate `typesafe-sdk`, v0.7.0 baseline). Installed as a library in other Rust projects.

## Rules
- Few dependencies. Allowed direct deps: `reqwest` (rustls), `serde`, `serde_json`, `tokio` (time only), `zstd`. Before you add any other crate, ask the user (std vs most common crate).
- Prefer std. Examples already done in std: random (`RandomState`), base64 decode, error enum (no `thiserror`).
- The TS SDK is the spec. Keep wire format, defaults, header names, and retry rules identical. If TS and Rust disagree, check the TS source first (clone it to the scratchpad).
- Mark deliberate gaps with a `ponytail:` comment that names the ceiling.
- Public API must not leak `Value` hacks where a type is clearer, but keep `serde_json::Value` for free-form instructions.

## Layout
- `src/client.rs`: `Client`, `ClientBuilder`, request loop (retry, compression, headers)
- `src/types.rs`: questions, answers, request, validation
- `src/assets.rs`: `Image`, reference scan, multipart encoding
- `src/retry.rs`: `RetryPolicy`, delay, `Retry-After`
- `src/error.rs`: `Error`, `ApiError`, message extraction
- `tests/client.rs`: end-to-end tests against a local TCP mock server

## Commands
```sh
cargo test
cargo clippy --all-targets
cargo fmt
```

## Vault
Project notes live in `~/projects/vault/Projects/typesafe-rust-sdk/` (plan: `Domain/Port plan.md`). Follow the global vault rules.
