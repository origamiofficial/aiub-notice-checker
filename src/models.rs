use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::UNIFIED_MAX_MESSAGE_LEN;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let mut sorted = self.attachments.clone();
        sorted.sort();
        sorted.dedup();
        serde_json::to_string(&sorted).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn content_hash(&self) -> String {
        let mut sorted_attachments = self.attachments.clone();
        sorted_attachments.sort();
        sorted_attachments.dedup();

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
            attachments: &sorted_attachments,
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
    pub tg_sent_at: Option<String>,
    pub tg_last_notified_hash: Option<String>,
    pub detail_checked_at: Option<String>,
    pub detail_fetched_at: Option<String>,
    pub fb_sent_at: Option<String>,
    pub fb_last_notified_hash: Option<String>,
}

impl NoticeRow {
    pub fn parsed_attachments(&self) -> Vec<String> {
        serde_json::from_str::<Vec<String>>(&self.attachments_json).unwrap_or_default()
    }

    pub fn detail_due(&self, retry_cutoff_iso: &str) -> bool {
        match (&self.detail_checked_at, &self.detail_fetched_at) {
            (Some(checked), None) => checked.as_str() <= retry_cutoff_iso,
            (Some(checked), Some(fetched)) if checked > fetched => {
                checked.as_str() <= retry_cutoff_iso
            }
            _ => true,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct D1Metrics {
    pub rows_read: usize,
    pub rows_written: usize,
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

pub fn is_meaningful_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let stripped = trimmed.trim_end_matches('.');
    !stripped.eq_ignore_ascii_case("no description available")
        && !stripped.eq_ignore_ascii_case("no description")
        && !stripped.eq_ignore_ascii_case("please click the link for details")
}

/// Formats a notice into the canonical unified plain-text message used across all platforms.
/// All visible messages delivered to Telegram, Facebook, or future channels are guaranteed
/// to be identical.
///
/// Plain text formatting:
/// - Zero HTML tags and zero Markdown syntax so it renders cleanly and natively across all platforms.
/// - Clean whitespace and blank lines between paragraphs and URLs.
/// - Preserves all attachment URLs and the main AIUB notice link at the bottom.
/// - Defensively caps length at `UNIFIED_MAX_MESSAGE_LEN` (4,096 chars), preserving header,
///   attachments, and the notice URL while gracefully truncating overflowing body text.
pub fn format_notice_message(notice: &Notice, edited: bool) -> String {
    let prefix = if edited { "[EDITED] " } else { "" };
    let title = truncate_text(&notice.title, 500);
    let date = display_date(notice.published_date.as_deref());

    let header = format!("{prefix}{title}\n\nDate: {date}");
    let notice_url = notice.url.trim();

    let valid_attachments: Vec<&str> = notice
        .attachments
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let text_content = if is_meaningful_text(&notice.body_text) {
        Some(notice.body_text.trim())
    } else if is_meaningful_text(&notice.description) {
        Some(notice.description.trim())
    } else {
        None
    };

    // Construct footer (attachments + notice URL), each on its own line
    let mut footer_parts = Vec::new();
    for att in &valid_attachments {
        footer_parts.push(*att);
    }
    footer_parts.push(notice_url);
    let footer = footer_parts.join("\n\n");

    let full_content = match (text_content, valid_attachments.is_empty()) {
        (Some(body), true) => {
            // Standard text notice without attachments
            format!("{header}\n\n{body}\n\n{notice_url}")
        }
        (Some(body), false) => {
            // Standard text notice with attachments
            format!("{header}\n\n{body}\n\n{footer}")
        }
        (None, false) => {
            // Image-only / flyer notice (attachment-first)
            format!("{header}\n\n{footer}")
        }
        (None, true) => {
            // No body and no attachments
            format!("{header}\n\nPlease click the link for details.\n\n{notice_url}")
        }
    };

    if full_content.chars().count() <= UNIFIED_MAX_MESSAGE_LEN {
        return full_content;
    }

    // Defensive truncation: truncate body text, but preserve header, attachments, and notice URL
    let suffix = format!("\n\n{footer}");
    let suffix_chars = suffix.chars().count();
    let header_chars = header.chars().count();
    let fixed_overhead = header_chars + suffix_chars + 4; // "\n\n" + "\n\n"

    let available_body_chars = UNIFIED_MAX_MESSAGE_LEN.saturating_sub(fixed_overhead);
    let truncated = if let Some(body) = text_content {
        let truncated_body = truncate_text(body, available_body_chars.max(10));
        format!("{header}\n\n{truncated_body}{suffix}")
    } else {
        truncate_text(&full_content, UNIFIED_MAX_MESSAGE_LEN)
    };

    if truncated.chars().count() > UNIFIED_MAX_MESSAGE_LEN {
        truncate_text(&truncated, UNIFIED_MAX_MESSAGE_LEN)
    } else {
        truncated
    }
}

/// Backwards-compatible alias for `format_notice_message`.
/// Guarantees that Facebook and future platforms receive the exact same unified visible text.
#[inline]
pub fn format_facebook_message(notice: &Notice, edited: bool) -> String {
    format_notice_message(notice, edited)
}
