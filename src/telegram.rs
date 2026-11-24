use serde::{Deserialize, Serialize};
use std::time::Duration;
use worker::{
    Delay, Headers, Method, Request, RequestInit, Result, console_log, console_warn,
    d1::{D1Database, D1DatabaseSession},
};

use crate::config::{
    HTTP_TIMEOUT_MS, Secrets, TELEGRAM_AUTH_PAUSE_TTL_HOURS, TELEGRAM_AUTH_PAUSED_KEY,
    TELEGRAM_MAX_RETRIES, TELEGRAM_MAX_RETRY_WAIT_SECS, TELEGRAM_PACE_MS,
};
use crate::db;
use crate::errors::AppError;
use crate::facebook::parse_auth_pause_value;

#[derive(Serialize)]
struct LinkPreviewOptions {
    is_disabled: bool,
}

#[derive(Serialize)]
struct SendMessagePayload<'a> {
    chat_id: &'a str,
    text: &'a str,
    // L7: the deprecated `disable_web_page_preview` field was removed;
    // `link_preview_options` is the only preview control sent.
    link_preview_options: LinkPreviewOptions,
}

#[derive(Deserialize)]
struct TelegramResponse {
    ok: bool,
    description: Option<String>,
    parameters: Option<TelegramResponseParams>,
}

#[derive(Deserialize)]
struct TelegramResponseParams {
    retry_after: Option<u64>,
}

/// Outcome of a Telegram send, distinguishing delivery from a
/// rate-limit deferral and from an auth failure (High #1). Callers must
/// handle `AuthFailure` by pausing Telegram dispatch — it must never be
/// treated as a transient error and retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramSendResult {
    /// Delivered to the chat.
    Sent,
    /// Rate-limited beyond the inline retry budget; defer to the next run.
    Deferred,
    /// HTTP 401/403: revoked/expired token, or the bot was blocked or
    /// removed. Do NOT retry — pause dispatch and alert the admin once.
    AuthFailure { message: String },
}

/// Sends a formatted notice alert to the main Telegram chat.
/// Returns `Sent` if delivered, `Deferred` if rate-limited beyond
/// `TELEGRAM_MAX_RETRY_WAIT_SECS`, or `AuthFailure` on HTTP 401/403.
///
/// O5: takes the pre-formatted message — `run()` formats each notice
/// once at queue time, and the Facebook phase uses the same string.
pub async fn send_notice_alert(secrets: &Secrets, message: &str) -> Result<TelegramSendResult> {
    send_raw_message(
        &secrets.telegram_bot_api_key,
        &secrets.telegram_chat_id,
        message,
    )
    .await
}

/// Sends an operational warning to `TELEGRAM_ADMIN_CHAT_ID` if configured.
pub async fn send_admin_alert(secrets: &Secrets, message: &str) -> Result<bool> {
    let Some(admin_chat_id) = secrets.telegram_admin_chat_id.as_deref() else {
        return Ok(false);
    };
    match send_raw_message(&secrets.telegram_bot_api_key, admin_chat_id, message).await? {
        TelegramSendResult::Sent => Ok(true),
        TelegramSendResult::Deferred => Ok(false),
        // An auth failure on the admin alert means the bot token itself
        // is bad — surface it as an error so the caller logs it instead
        // of silently treating it as "not delivered".
        TelegramSendResult::AuthFailure { message } => {
            Err(AppError::Fetch(format!("Telegram admin alert auth failure: {message}")).into())
        }
    }
}

/// Checks whether Telegram dispatch is temporarily paused due to an auth
/// failure (High #1). Mirrors `facebook::is_facebook_paused`: a pause
/// older than `TELEGRAM_AUTH_PAUSE_TTL_HOURS` (24h) — or a legacy value
/// with no timestamp — is deleted and treated as not paused, so a
/// transient blip cannot kill Telegram posting forever. A genuine auth
/// failure re-pauses on the next failed send.
pub async fn is_telegram_paused(
    db: &D1Database,
    session: &D1DatabaseSession,
) -> Result<(bool, Option<String>)> {
    let Some(reason) = db::get_app_state(session, TELEGRAM_AUTH_PAUSED_KEY).await? else {
        return Ok((false, None));
    };
    match parse_auth_pause_value(&reason) {
        Some((paused_at, message)) => {
            let age = chrono::Utc::now().signed_duration_since(paused_at);
            if age.num_hours() >= TELEGRAM_AUTH_PAUSE_TTL_HOURS {
                console_log!(
                    "Telegram auth pause expired (paused at {}, {}h old); clearing it.",
                    paused_at.to_rfc3339(),
                    age.num_hours()
                );
                if let Err(err) = db::delete_app_state(db, session, TELEGRAM_AUTH_PAUSED_KEY).await
                {
                    console_warn!(
                        "Failed to clear expired Telegram auth pause: {err}; the pause will be re-checked on the next run."
                    );
                }
                Ok((false, None))
            } else {
                Ok((true, Some(format!("Authentication paused: {message}"))))
            }
        }
        None => {
            // Legacy value with no timestamp: its age is unknowable, so
            // it must not be allowed to pause posting forever. Clear it;
            // a real auth failure will re-pause on the next attempt.
            console_log!("Telegram auth pause has no timestamp (legacy value); clearing it.");
            if let Err(err) = db::delete_app_state(db, session, TELEGRAM_AUTH_PAUSED_KEY).await {
                console_warn!(
                    "Failed to clear legacy Telegram auth pause: {err}; the pause will be re-checked on the next run."
                );
            }
            Ok((false, None))
        }
    }
}

/// Pure classification of a Telegram Bot API HTTP status (Phase C,
/// Medium #10): 2xx is delivery, 429 is a rate limit, 401/403 is an
/// auth failure (High #1 — revoked token, or the bot was blocked or
/// removed), anything else is a plain error. Kept separate from
/// `send_raw_message` (which does network I/O) so the 401/403 rule and
/// the bounded-429 rule are unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TelegramStatusAction {
    Success,
    RateLimited,
    AuthFailure,
    Error,
}

#[must_use]
pub(crate) fn classify_telegram_status(status: u16) -> TelegramStatusAction {
    if (200..300).contains(&status) {
        TelegramStatusAction::Success
    } else if status == 429 {
        TelegramStatusAction::RateLimited
    } else if status == 401 || status == 403 {
        TelegramStatusAction::AuthFailure
    } else {
        TelegramStatusAction::Error
    }
}

/// Pure 2xx delivery check (audit #3): the Bot API's `ok` field must
/// be true — a 2xx carrying `{"ok":false,...}` (e.g. the bot was
/// blocked) is not a delivery, and an unparsable body proves nothing
/// either. `false` means the caller falls through to the error/auth
/// handling below, using `description`, so the notice is retried
/// instead of recorded as sent.
#[must_use]
pub(crate) fn telegram_body_delivered(raw_body: &str) -> bool {
    serde_json::from_str::<TelegramResponse>(raw_body)
        .map(|r| r.ok)
        .unwrap_or(false)
}

/// Pure inline-retry decision for a Telegram 429 (Phase C, Medium #10):
/// retry inline only while attempts remain AND the server's
/// `retry_after` fits the inline budget (`TELEGRAM_MAX_RETRY_WAIT_SECS`);
/// otherwise defer to the next run. The bound is what keeps a 429 from
/// retrying forever — `TELEGRAM_MAX_RETRIES` attempts, at most 10s each.
#[must_use]
pub(crate) fn should_retry_telegram_429(attempt: usize, retry_after_secs: u64) -> bool {
    attempt < TELEGRAM_MAX_RETRIES && retry_after_secs <= TELEGRAM_MAX_RETRY_WAIT_SECS
}

async fn send_raw_message(
    bot_api_key: &str,
    chat_id: &str,
    text: &str,
) -> Result<TelegramSendResult> {
    let url = format!("https://api.telegram.org/bot{bot_api_key}/sendMessage");

    let payload = SendMessagePayload {
        chat_id,
        text,
        link_preview_options: LinkPreviewOptions { is_disabled: true },
    };

    let body_json = serde_json::to_string(&payload)
        .map_err(|e| AppError::Fetch(format!("Failed to serialize Telegram payload: {e}")))?;

    for attempt in 0..=TELEGRAM_MAX_RETRIES {
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            // O10: no `body_json.clone()` per retry — the body is built
            // once above, and `&str -> JsValue` creates the JS string
            // directly from the borrowed slice.
            .with_body(Some(body_json.as_str().into()));

        let req = Request::new_with_init(&url, &init)?;
        // Audit #19: the shared fetch-with-timeout skeleton — one
        // 15s deadline for send + body read, with the caller's error
        // mapping (send errors are token-redacted).
        let pending = match crate::fetch::send_with_timeout(req, HTTP_TIMEOUT_MS).await {
            crate::fetch::SendOutcome::Sent(pending) => pending,
            crate::fetch::SendOutcome::SendFailed(e) => {
                return Err(AppError::Fetch(redact_token(&e.to_string(), bot_api_key)).into());
            }
            crate::fetch::SendOutcome::SendTimedOut => {
                return Err(AppError::Fetch(format!(
                    "Telegram API timeout after {HTTP_TIMEOUT_MS}ms"
                ))
                .into());
            }
        };

        let status = pending.response().status_code();
        // M9: the 15s timeout must cover the body read too, not just
        // `send()` — a stalled body stream previously hung the send past
        // the timeout. On timeout, fail the attempt (the caller retries
        // on the next run) instead of blocking the worker. The deadline
        // is the same one started above, so send + body share 15s.
        let raw_body = match pending.read_body().await {
            crate::fetch::BodyOutcome::Read(_resp, body) => body.unwrap_or_default(),
            crate::fetch::BodyOutcome::TimedOut => {
                return Err(AppError::Fetch(format!(
                    "Telegram API timeout after {HTTP_TIMEOUT_MS}ms reading response body"
                ))
                .into());
            }
        };

        let action = classify_telegram_status(status);
        if action == TelegramStatusAction::Success && telegram_body_delivered(&raw_body) {
            Delay::from(Duration::from_millis(TELEGRAM_PACE_MS)).await;
            return Ok(TelegramSendResult::Sent);
        }

        if action == TelegramStatusAction::RateLimited {
            // `retry_after` is parsed from the raw body (not `desc` —
            // the description string is not valid Telegram JSON).
            let retry_after = serde_json::from_str::<TelegramResponse>(&raw_body)
                .ok()
                .and_then(|r| r.parameters)
                .and_then(|p| p.retry_after)
                .unwrap_or(TELEGRAM_MAX_RETRY_WAIT_SECS);

            if should_retry_telegram_429(attempt, retry_after) {
                console_warn!(
                    "Telegram 429 rate limit; waiting {}s before retry...",
                    retry_after
                );
                Delay::from(Duration::from_secs(retry_after)).await;
                continue;
            } else {
                console_warn!(
                    "Telegram 429 retry_after={}s exceeds inline budget; deferring.",
                    retry_after
                );
                return Ok(TelegramSendResult::Deferred);
            }
        }

        let desc = serde_json::from_str::<TelegramResponse>(&raw_body)
            .ok()
            .and_then(|r| r.description)
            .unwrap_or(raw_body);

        // High #1: HTTP 401/403 is an auth failure (revoked token, or
        // the bot was blocked/removed from the chat). Break the retry
        // loop immediately — retrying a dead token only burns runs —
        // and report it so the caller pauses Telegram dispatch and
        // alerts the admin once.
        if action == TelegramStatusAction::AuthFailure {
            let message = format!("Telegram API HTTP {status}: {desc}");
            return Ok(TelegramSendResult::AuthFailure {
                message: redact_token(&message, bot_api_key),
            });
        }

        let err_msg = format!("Telegram API HTTP {status}: {desc}");
        return Err(AppError::Fetch(redact_token(&err_msg, bot_api_key)).into());
    }

    Ok(TelegramSendResult::Deferred)
}

/// Percent-encodes a token the way it would appear inside a URL/query string.
fn url_encode_token(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for b in token.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Replaces any run starting with `EAA` followed by alphanumerics (the shape
/// of a Meta access token) with `[REDACTED]`.
fn redact_eaa_tokens(msg: &str) -> String {
    let bytes = msg.as_bytes();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < bytes.len() {
        if msg[i..].starts_with("EAA") {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            // Only redact if there is a real token body after the prefix.
            if j > i + 3 {
                out.push_str("[REDACTED]");
                i = j;
                continue;
            }
        }
        // Copy one UTF-8 char.
        let ch_len = msg[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&msg[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Redacts a secret token from an error string (L3, shared by Telegram and
/// Facebook callers). Covers the exact token, its URL-encoded form, a
/// truncated fragment (first/last 20 chars, when the token is long enough
/// for the fragment to be distinctive), and any `EAA...` Meta token pattern.
pub(crate) fn redact_token(msg: &str, bot_api_key: &str) -> String {
    let key = bot_api_key.trim();
    if key.is_empty() {
        return redact_eaa_tokens(msg);
    }
    let mut redacted = msg.replace(key, "[REDACTED]");
    let encoded = url_encode_token(key);
    if encoded != key {
        redacted = redacted.replace(&encoded, "[REDACTED]");
    }
    if key.chars().count() >= 20 {
        // A truncated error string may only contain the head or tail of the
        // token; redact those distinctive fragments too.
        let head: String = key.chars().take(20).collect();
        let tail: String = key
            .chars()
            .rev()
            .take(20)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        redacted = redacted.replace(&head, "[REDACTED]");
        if tail != head {
            redacted = redacted.replace(&tail, "[REDACTED]");
        }
    }
    redact_eaa_tokens(&redacted)
}
