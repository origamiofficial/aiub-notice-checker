use serde::{Deserialize, Serialize};
use worker::{console_error, Fetch, Headers, Method, Request, RequestInit, Result};

#[derive(Serialize)]
struct SendMessagePayload<'a> {
    chat_id: &'a str,
    text: &'a str,
    disable_web_page_preview: bool,
}

#[derive(Deserialize)]
struct TelegramResponse {
    ok: bool,
    error_code: Option<u16>,
    description: Option<String>,
}

pub async fn send_message(bot_api_key: &str, chat_id: &str, text: &str) -> Result<bool> {
    let url = format!("https://api.telegram.org/bot{bot_api_key}/sendMessage");
    let body = serde_json::to_string(&SendMessagePayload {
        chat_id,
        text,
        disable_web_page_preview: true,
    })
    .map_err(|e| worker::Error::RustError(format!("JSON serialize error: {e}")))?;

    let mut headers = Headers::new();
    headers.set("Content-Type", "application/json")?;

    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    init.with_headers(headers);
    init.with_body(Some(body.into()));

    let req = Request::new_with_init(&url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let status = resp.status_code();
    let parsed = resp.json::<TelegramResponse>().await.ok();

    match parsed {
        Some(res) if status == 200 && res.ok => Ok(true),
        Some(res) => {
            let code = res.error_code.unwrap_or(status);
            let desc = res
                .description
                .unwrap_or_else(|| "unknown error".to_string())
                .replace(bot_api_key, "<redacted>");
            console_error!("Telegram API error ({code}): {desc}");
            Ok(false)
        }
        None => {
            console_error!("Telegram HTTP {status} with non-JSON response");
            Ok(false)
        }
    }
}

pub async fn send_admin_alert(bot_api_key: &str, admin_chat_id: Option<&str>, message: &str) {
    if let Some(chat_id) = admin_chat_id {
        let _ = send_message(bot_api_key, chat_id, message).await;
    }
}