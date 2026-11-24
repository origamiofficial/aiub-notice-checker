//! One shared fetch-with-timeout skeleton (audit #19).
//!
//! The scraper, Telegram and Facebook clients each pasted the same
//! dance — build a timeout delay, race `send()` against it, abort on
//! timeout, race the body read against the same timeout. One shared
//! deadline covers send AND the body read: a fresh Delay per phase
//! previously allowed one fetch to take up to 30s.
//!
//! The seam is two-phase on purpose: the scraper inspects the
//! response between send and body read (redirects, non-2xx,
//! Content-Length pre-check), so a single send+body helper would
//! read the body first and defeat the M9 size pre-check. Callers do
//! `send_with_timeout`, inspect `pending.response()`, then drive the
//! body read with `pending.read_body()` under the same deadline.
//! Callers map the failure outcomes to their own error type:
//! scraper → AppError, Telegram → redacted-token error, Facebook →
//! TransientFailure. The body read's own error rides inside
//! `BodyOutcome::Read` so the scraper can propagate it (`?`) while
//! Telegram and Facebook default it away, exactly as before.

use std::pin::pin;
use std::time::Duration;

use futures_util::future::{Either, select};
use worker::{AbortController, Delay, Error, Fetch, Request, Response};

/// What a timed send produced.
pub enum SendOutcome {
    /// The send completed within the deadline. The body has not been
    /// read yet — inspect the response, then call `read_body()`.
    Sent(PendingBody),
    /// The send itself failed (network/worker error).
    SendFailed(Error),
    /// The shared deadline fired during the send.
    SendTimedOut,
}

/// A sent response whose body is still unread, with the shared
/// deadline and abort controller carried over from the send.
pub struct PendingBody {
    resp: Response,
    controller: AbortController,
    timeout: std::pin::Pin<Box<Delay>>,
}

impl PendingBody {
    /// The response, for status/header inspection before the body read.
    #[must_use]
    pub fn response(&self) -> &Response {
        &self.resp
    }

    /// Reads the body under the same deadline the send used.
    pub async fn read_body(self) -> BodyOutcome {
        let Self {
            mut resp,
            controller,
            mut timeout,
        } = self;
        // The body-read select lives in its own block: `pin!` keeps
        // the borrow of `resp` alive until the end of the enclosing
        // statement, so `resp` can only move out after the block ends.
        let body = {
            let text_fut = resp.text();
            match select(pin!(text_fut), timeout.as_mut()).await {
                Either::Left((res, _)) => Some(res),
                Either::Right(((), _)) => None,
            }
        };
        match body {
            Some(res) => BodyOutcome::Read(resp, res),
            None => {
                controller.abort();
                BodyOutcome::TimedOut
            }
        }
    }
}

/// What a timed body read produced.
pub enum BodyOutcome {
    /// The body read finished. The body is a `Result` because its
    /// read can fail independently of the send.
    Read(Response, Result<String, Error>),
    /// The shared deadline fired during the body read.
    TimedOut,
}

/// Sends one request under a shared deadline covering the send and
/// the later body read; aborts the request on timeout. `select`
/// returns the losing future, so the same Delay is reused for the
/// body read — `timeout` is boxed and pinned (not `pin!(...)`
/// inline) so it can live in the returned `PendingBody`.
pub async fn send_with_timeout(req: Request, timeout_ms: u64) -> SendOutcome {
    let controller = AbortController::default();
    let signal = controller.signal();

    let fetch = Fetch::Request(req);
    let send_fut = fetch.send_with_signal(&signal);
    let mut timeout = Box::pin(Delay::from(Duration::from_millis(timeout_ms)));

    match select(pin!(send_fut), timeout.as_mut()).await {
        Either::Left((res, _)) => match res {
            Ok(resp) => SendOutcome::Sent(PendingBody {
                resp,
                controller,
                timeout,
            }),
            Err(e) => SendOutcome::SendFailed(e),
        },
        Either::Right(((), _)) => {
            controller.abort();
            SendOutcome::SendTimedOut
        }
    }
}
