use std::time::Duration;

use serde::{Deserialize, Serialize};
use worker::{console_error, Delay, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::config::{
    HTTP_TIMEOUT_MS, TELEGRAM_MAX_RETRIES, TELEGRAM_MAX_RETRY_WAIT_SECS, TELEGRAM_PACE_MS,
};
use crate::scraper::send_with_timeout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Delivered,
    Failed,
    RateLimited,
}

#[derive(Serialize)]
struct SendMessagePayload<'a> {
    chat_id: &'a str,
    text: &'a str,
    disable_web_page_preview: bool,
}

#[derive(Deserialize)]
struct TelegramParameters {
    retry_after: Option<u64>,
}

#[derive(Deserialize)]
struct TelegramResponse {
    ok: bool,
    error_code: Option<u16>,
    description: Option<String>,
    parameters: Option<TelegramParameters>,
}

pub async fn pace_between_messages() {
    Delay::from(Duration::from_millis(TELEGRAM_PACE_MS)).await;
}

pub async fn send_message(bot_api_key: &str, chat_id: &str, text: &str) -> Result<SendOutcome> {
    let url = format!("https://api.telegram.org/bot{bot_api_key}/sendMessage");
    let body = serde_json::to_string(&SendMessagePayload {
        chat_id,
        text,
        disable_web_page_preview: true,
    })
    .map_err(|e| worker::Error::RustError(format!("JSON serialize error: {e}")))?;

    for attempt in 0..=TELEGRAM_MAX_RETRIES {
        let mut headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut init = RequestInit::new();
        init.with_method(Method::Post);
        init.with_headers(headers);
        init.with_body(Some(body.clone().into()));

        let req = Request::new_with_init(&url, &init)?;
        let mut resp = match send_with_timeout(Fetch::Request(req), HTTP_TIMEOUT_MS).await {
            Ok(r) => r,
            Err(err) => {
                let redacted = err.to_string().replace(bot_api_key, "<redacted>");
                console_error!("Telegram transport error: {redacted}");
                return Ok(SendOutcome::Failed);
            }
        };

        let status = resp.status_code();
        let parsed = resp.json::<TelegramResponse>().await.ok();

        match parsed {
            Some(res) if status == 200 && res.ok => return Ok(SendOutcome::Delivered),
            Some(res) if status == 429 || res.error_code == Some(429) => {
                let retry_after = res
                    .parameters
                    .and_then(|p| p.retry_after)
                    .unwrap_or(2 * (attempt as u64 + 1));

                if retry_after > TELEGRAM_MAX_RETRY_WAIT_SECS || attempt == TELEGRAM_MAX_RETRIES {
                    console_error!(
                        "Telegram rate-limited (retry_after={retry_after}s); deferring remaining messages."
                    );
                    return Ok(SendOutcome::RateLimited);
                }

                Delay::from(Duration::from_secs(retry_after.max(1))).await;
                continue;
            }
            Some(res) => {
                let code = res.error_code.unwrap_or(status);
                let desc = res
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
                    .replace(bot_api_key, "<redacted>");
                console_error!("Telegram API error ({code}): {desc}");
                return Ok(SendOutcome::Failed);
            }
            None => {
                console_error!("Telegram HTTP {status} with non-JSON response");
                return Ok(SendOutcome::Failed);
            }
        }
    }

    Ok(SendOutcome::Failed)
}

pub async fn send_admin_alert(bot_api_key: &str, admin_chat_id: Option<&str>, message: &str) {
    if let Some(chat_id) = admin_chat_id {
        let _ = send_message(bot_api_key, chat_id, message).await;
    }
}