# typesafe-sdk

Unofficial Rust client for the [TypeSafe AI](https://typesafe.ai) API. It is a port of the
official JavaScript SDK, [`typesafe-sdk-js`](https://github.com/typesafe-ai/typesafe-sdk-js)
(ported from v0.7.0).

Async, built on `reqwest` + `serde_json` + `zstd`. No other direct dependencies.

## Install

```toml
[dependencies]
typesafe-sdk = { git = "https://github.com/makbari/typesafe-rust-sdk" }
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Quickstart

Set `TYPESAFE_API_KEY`, then:

```rust
use serde_json::Value;
use typesafe_sdk::{choice, Client, SystemOneRequest};

#[tokio::main]
async fn main() -> typesafe_sdk::Result<()> {
    let client = Client::new()?;
    let result = client
        .system_one(
            SystemOneRequest::new("I was charged twice. Please fix this ASAP.").question(
                "category",
                choice(
                    "What is this ticket about?",
                    [("billing", Value::Null), ("technical", Value::Null), ("other", Value::Null)],
                ),
            ),
        )
        .await?;
    println!("{}", result.answers["category"].as_choice().unwrap().choice);
    Ok(())
}
```

Rust cannot infer answer types from question values like TypeScript can. Answers come back as
an `Answer` enum keyed by question name; use `as_noul()`, `as_choice()`, `as_score()`, and
`as_box()`.

## Questions

| TS | Rust | Answer |
|---|---|---|
| `noul(...)` | `noul(...)` | `NoulAnswer { noul }` |
| `choice(...)` | `choice(...)` | `ChoiceAnswer { choice, confidence, probabilities }` |
| `score(...)` | `score(...)` | `ScoreAnswer { score, confidence, legend, probabilities }` |
| `box(...)` | `locate(...)` | `BoxAnswer { bounds, confidence }` |

`SystemOneRequest::new(state).question(name, q).model("jev-preview")`. Invalid questions (none,
a score rubric under two entries, an empty choice, a noul with no text) fail before sending.

## Images

Image support is experimental on the API side. Send the opt-in header:

```rust
let client = Client::builder().default_header("TypeSafe-Danger-Zone-Images", "true").build()?;
```

`Image` holds PNG or JPEG bytes. Its `Display` text is its reference, so `format!` replaces
the TS `withAssets` template tag. Attach the image with `.image(&img)`:

```rust
let photo = Image::new(std::fs::read("returns/RMA-1042.jpg")?)?;
let req = SystemOneRequest::new(format!("The mug arrived chipped: {photo}"))
    .image(&photo)
    .question("damaged", noul(format!("Is the product in {photo} damaged?")));
```

Or choose your own reference with `.asset("<photo>", &img)` and write `<photo>` in the text.
Images go out as `multipart/form-data`, once each.

## Configuration

| Option | Env var | Default |
|---|---|---|
| `api_key` | `TYPESAFE_API_KEY` | required |
| `base_url` | `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` |
| `default_model` | `TYPESAFE_DEFAULT_MODEL` | `jev-latest` |
| `timeout` | - | 10 s per attempt |
| `retry` | - | 2 retries, 500 ms to 5 s backoff, 25% jitter; 408/429/5xx, connection errors, timeouts; honors `Retry-After` |
| `compression` | - | zstd for bodies of 1 KiB or more |

Per-call: `client.system_one_with(req, &RequestOptions::default().header(...))`. Drop the future to cancel.

## Errors

`Error::Api(ApiError)` carries `kind`, `status`, `body`, `headers`, and `request_id`.
Other variants: `Sdk` (bad input or config), `Connection`, `Timeout`.

## Differences from the JS SDK

Not yet ported: logging and `TYPESAFE_LOG_LEVEL`, HTTP-date `Retry-After`, field-wise retry
overrides. Browser guard and `fetch` injection do not apply (use `ClientBuilder::http_client`).
See the plan in the project notes for the roadmap.

## Development

```sh
cargo test
cargo clippy --all-targets
```

MIT licensed. Not affiliated with TypeSafe AI.
