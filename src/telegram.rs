use futures_util::future::{select, Either};
use serde::{Deserialize, Serialize};
use std::pin::pin;
use std::time::Duration;
use worker::{
    console_warn, AbortController, Delay, Error, Fetch, Headers, Method, Request, RequestInit,
    Result,
};

use crate::config::{
    Secrets, HTTP_TIMEOUT_MS, TELEGRAM_MAX_RETRIES, TELEGRAM_MAX_RETRY_WAIT_SECS, TELEGRAM_PACE_MS,
};
use crate::models::{format_notice_message, Notice};

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
    #[allow(dead_code)]
    ok: bool,
    description: Option<String>,
    parameters: Option<TelegramResponseParams>,
}

#[derive(Deserialize)]
struct TelegramResponseParams {
    retry_after: Option<u64>,
}

/// Sends a formatted notice alert to the main Telegram chat.
/// Returns `Ok(true)` if delivered, or `Ok(false)` if rate-limited beyond `TELEGRAM_MAX_RETRY_WAIT_SECS`.
pub async fn send_notice_alert(secrets: &Secrets, notice: &Notice, edited: bool) -> Result<bool> {
    let text = format_notice_message(notice, edited);
    send_raw_message(
        &secrets.telegram_bot_api_key,
        &secrets.telegram_chat_id,
        &text,
    )
    .await
}

/// Sends an operational warning to `TELEGRAM_ADMIN_CHAT_ID` if configured.
pub async fn send_admin_alert(secrets: &Secrets, message: &str) -> Result<bool> {
    let Some(admin_chat_id) = secrets.telegram_admin_chat_id.as_deref() else {
        return Ok(false);
    };
    send_raw_message(&secrets.telegram_bot_api_key, admin_chat_id, message).await
}

async fn send_raw_message(bot_api_key: &str, chat_id: &str, text: &str) -> Result<bool> {
    let url = format!("https://api.telegram.org/bot{bot_api_key}/sendMessage");

    let payload = SendMessagePayload {
        chat_id,
        text,
        link_preview_options: LinkPreviewOptions { is_disabled: true },
    };

    let body_json = serde_json::to_string(&payload)
        .map_err(|e| Error::RustError(format!("Failed to serialize Telegram payload: {e}")))?;

    for attempt in 0..=TELEGRAM_MAX_RETRIES {
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(body_json.clone().into()));

        let req = Request::new_with_init(&url, &init)?;
        let controller = AbortController::default();
        let signal = controller.signal();

        let fetch = Fetch::Request(req);
        let send_fut = fetch.send_with_signal(&signal);
        let timeout_fut = Delay::from(Duration::from_millis(HTTP_TIMEOUT_MS));

        let mut resp = match select(pin!(send_fut), pin!(timeout_fut)).await {
            Either::Left((res, _)) => match res {
                Ok(r) => r,
                Err(e) => {
                    return Err(Error::RustError(redact_token(&e.to_string(), bot_api_key)));
                }
            },
            Either::Right(((), _)) => {
                controller.abort();
                return Err(Error::RustError(format!(
                    "Telegram API timeout after {}ms",
                    HTTP_TIMEOUT_MS
                )));
            }
        };

        let status = resp.status_code();
        // M9: the 15s timeout must cover the body read too, not just
        // `send()` — a stalled body stream previously hung the send past
        // the timeout. On timeout, fail the attempt (the caller retries
        // on the next run) instead of blocking the worker.
        let text_fut = resp.text();
        let timeout_fut = Delay::from(Duration::from_millis(HTTP_TIMEOUT_MS));
        let raw_body = match select(pin!(text_fut), pin!(timeout_fut)).await {
            Either::Left((res, _)) => res.unwrap_or_default(),
            Either::Right(((), _)) => {
                controller.abort();
                return Err(Error::RustError(format!(
                    "Telegram API timeout after {}ms reading response body",
                    HTTP_TIMEOUT_MS
                )));
            }
        };

        if (200..300).contains(&status) {
            Delay::from(Duration::from_millis(TELEGRAM_PACE_MS)).await;
            return Ok(true);
        }

        if status == 429 {
            let retry_after = serde_json::from_str::<TelegramResponse>(&raw_body)
                .ok()
                .and_then(|r| r.parameters)
                .and_then(|p| p.retry_after)
                .unwrap_or(TELEGRAM_MAX_RETRY_WAIT_SECS);

            if attempt < TELEGRAM_MAX_RETRIES && retry_after <= TELEGRAM_MAX_RETRY_WAIT_SECS {
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
                return Ok(false);
            }
        }

        let desc = serde_json::from_str::<TelegramResponse>(&raw_body)
            .ok()
            .and_then(|r| r.description)
            .unwrap_or(raw_body);

        let err_msg = format!("Telegram API HTTP {}: {}", status, desc);
        return Err(Error::RustError(redact_token(&err_msg, bot_api_key)));
    }

    Ok(false)
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
