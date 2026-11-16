use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::TELEGRAM_MAX_MESSAGE_LEN;

#[derive(Debug, Clone)]
pub struct Notice {
    pub title: String,
    pub description: String,
    pub url: String,
    pub published_date: Option<String>,
    pub body_text: String,
    pub attachments: Vec<String>,
    pub detail_fetched: bool,
}

impl Notice {
    pub fn attachments_json(&self) -> String {
        serde_json::to_string(&self.attachments).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn content_hash(&self) -> String {
        #[derive(Serialize)]
        struct HashPayload<'a> {
            attachments: &'a [String],
            body_text: &'a str,
            description: &'a str,
            link: &'a str,
            published_date: &'a str,
            title: &'a str,
        }

        let payload = HashPayload {
            attachments: &self.attachments,
            body_text: &self.body_text,
            description: &self.description,
            link: &self.url,
            published_date: self.published_date.as_deref().unwrap_or(""),
            title: &self.title,
        };

        let serialized = serde_json::to_string(&payload).unwrap_or_default();
        let digest = Sha256::digest(serialized.as_bytes());
        hex::encode(digest)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NoticeRow {
    pub title: String,
    pub description: String,
    pub link: String,
    pub published_date: Option<String>,
    pub body_text: String,
    pub attachments_json: String,
    pub content_hash: String,
    pub sent_at: Option<String>,
    pub last_notified_hash: Option<String>,
    pub detail_checked_at: Option<String>,
    pub detail_fetched_at: Option<String>,
}

impl NoticeRow {
    pub fn parsed_attachments(&self) -> Vec<String> {
        serde_json::from_str::<Vec<String>>(&self.attachments_json).unwrap_or_default()
    }
}

pub fn titles_equivalent(listing_title: &str, cached_title: &str) -> bool {
    let normalize = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    if normalize(listing_title) == normalize(cached_title) {
        return true;
    }
    listing_title.contains("...")
}

pub fn truncate_text(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let keep = limit.saturating_sub(3);
    let prefix: String = trimmed.chars().take(keep).collect();
    format!("{}...", prefix.trim_end())
}

pub fn display_date(iso_date: Option<&str>) -> String {
    match iso_date {
        Some(raw) => match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            Ok(d) => d.format("%d %b %Y").to_string(),
            Err(_) => raw.to_string(),
        },
        None => "Unknown".to_string(),
    }
}

pub fn format_notice_message(notice: &Notice, edited: bool) -> String {
    let prefix = if edited { "[EDITED] " } else { "" };
    let header = format!(
        "{prefix}{}\n\nDate: {}\n\n",
        truncate_text(&notice.title, 500),
        display_date(notice.published_date.as_deref())
    );
    let suffix = format!("\n\n{}", notice.url);

    let raw_body = if !notice.body_text.is_empty() {
        notice.body_text.as_str()
    } else if !notice.description.is_empty() {
        notice.description.as_str()
    } else {
        "Please click the link for details."
    };

    let available = TELEGRAM_MAX_MESSAGE_LEN
        .saturating_sub(header.chars().count())
        .saturating_sub(suffix.chars().count());
    let body = truncate_text(raw_body, available.min(1600).max(4));
    format!("{header}{body}{suffix}")
}