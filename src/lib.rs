//! Rust client for the [TypeSafe AI](https://typesafe.ai) API.
//!
//! A port of the official JavaScript SDK (`@typesafe-ai/sdk`).
//!
//! ```no_run
//! use typesafe_sdk::{choice, Client, SystemOneRequest};
//! use serde_json::Value;
//!
//! # async fn run() -> typesafe_sdk::Result<()> {
//! let client = Client::new()?; // reads TYPESAFE_API_KEY
//! let result = client
//!     .system_one(
//!         SystemOneRequest::new("I was charged twice. Please fix this ASAP.").question(
//!             "category",
//!             choice(
//!                 "What is this ticket about?",
//!                 [("billing", Value::Null), ("technical", Value::Null), ("other", Value::Null)],
//!             ),
//!         ),
//!     )
//!     .await?;
//! println!("{}", result.answers["category"].as_choice().unwrap().choice);
//! # Ok(())
//! # }
//! ```

mod assets;
mod client;
mod error;
mod retry;
mod types;

pub use assets::Image;
pub use client::{
    Client, ClientBuilder, Compression, DEFAULT_BASE_URL, DEFAULT_MODEL, RequestOptions,
};
pub use error::{ApiError, ApiErrorKind, Error, Result};
pub use retry::RetryPolicy;
pub use types::{
    Answer, BoxAnswer, BoxCoordinates, ChoiceAnswer, ModelCard, NoulAnswer, NoulCriteria, Question,
    ScoreAnswer, SystemOneRequest, SystemOneResult, Usage, choice, locate, noul, score,
};
