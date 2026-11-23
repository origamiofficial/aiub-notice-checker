//! Hand-written internal error taxonomy (Phase B, Medium #3).
//!
//! `worker::Error` is kept at the Worker boundary (`run()`, `scheduled()`),
//! but internal layers classify failures with [`AppError`] so callers can
//! tell a fetch failure from a D1 failure from a config failure without
//! string-matching. No `thiserror`/`anyhow`: the enum is small and the
//! `Display` impl is written by hand.
//!
//! Only the sites touched by Phase B (config loading, fetch, D1 writes in
//! the dispatch path) construct `AppError` today; the rest of the codebase
//! still returns `worker::Error` directly. Convert a call site when you
//! touch it — do not rewrite the whole tree in one go.

use std::fmt;
use worker::Error;

/// Internal error taxonomy for the fetch / D1 / config layers.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// An HTTP fetch failed (network error, timeout, non-2xx status,
    /// oversized body). Carries a redacted, log-safe message.
    Fetch(String),
    /// A D1 read/write/batch failed.
    D1(String),
    /// A required secret or configuration value is missing or invalid.
    /// Carries the underlying source error in the message.
    Config(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(msg) => write!(f, "fetch error: {msg}"),
            Self::D1(msg) => write!(f, "D1 error: {msg}"),
            Self::Config(msg) => write!(f, "config error: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

/// Boundary conversion: an `AppError` becomes a `worker::Error` at the
/// point a `worker::Result` is returned to the runtime.
impl From<AppError> for Error {
    fn from(err: AppError) -> Self {
        Error::RustError(err.to_string())
    }
}
