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
struct SendMessagePayload<'a> {
    chat_id: &'a str,
    text: &'a str,
    disable_web_page_preview: bool,
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
        disable_web_page_preview: false,
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
            Either::Left((res, _)) => res?,
            Either::Right(((), _)) => {
                controller.abort();
                return Err(Error::RustError(format!(
                    "Telegram API timeout after {}ms",
                    HTTP_TIMEOUT_MS
                )));
            }
        };

        let status = resp.status_code();
        let raw_body = resp.text().await.unwrap_or_default();

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

        return Err(Error::RustError(format!(
            "Telegram API HTTP {}: {}",
            status, desc
        )));
    }

    Ok(false)
}