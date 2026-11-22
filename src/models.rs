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
        let mut sorted: Vec<&str> = self.attachments.iter().map(|s| s.as_str()).collect();
        sorted.sort_unstable();
        sorted.dedup();
        serde_json::to_string(&sorted).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn content_hash(&self) -> String {
        let mut sorted_attachments: Vec<&str> =
            self.attachments.iter().map(|s| s.as_str()).collect();
        sorted_attachments.sort_unstable();
        sorted_attachments.dedup();

        #[derive(Serialize)]
        struct HashPayload<'a> {
            attachments: &'a [&'a str],
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

/// Length of a string in UTF-16 code units — the unit Telegram (and
/// Facebook) count towards the 4,096 message limit. Rust `char`s are NOT
/// equivalent: one emoji / non-BMP character is 1 `char` but 2 UTF-16 units
/// (M5).
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Takes the longest prefix of `text` whose UTF-16 length is at most
/// `max_units`, never splitting a character.
fn take_utf16_prefix(text: &str, max_units: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let units = c.len_utf16();
        if used + units > max_units {
            break;
        }
        out.push(c);
        used += units;
    }
    out
}

/// Truncates `text` to at most `limit` UTF-16 code units (M5), appending
/// `...` when truncated. The result never exceeds `limit` UTF-16 units.
pub fn truncate_text(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if utf16_len(trimmed) <= limit {
        return trimmed.to_string();
    }
    if limit <= 3 {
        return take_utf16_prefix(trimmed, limit);
    }
    let keep = limit.saturating_sub(3);
    let prefix = take_utf16_prefix(trimmed, keep);
    format!("{}...", prefix.trim_end())
}

/// Assembles `header + footer` (attachments + notice URL) within
/// `UNIFIED_MAX_MESSAGE_LEN` UTF-16 units, guaranteeing the footer — in
/// particular the notice URL — is never truncated (M5). If the header and
/// footer together are too long, attachments are dropped first (last one
/// first), and only then is the header truncated.
fn assemble_header_footer(header: &str, footer_parts: &[String]) -> String {
    let mut parts: Vec<String> = footer_parts.to_vec();
    loop {
        let footer = parts.join("\n\n");
        let total = utf16_len(header) + 2 + utf16_len(&footer);
        if total <= UNIFIED_MAX_MESSAGE_LEN {
            return format!("{header}\n\n{footer}");
        }
        if parts.len() > 1 {
            // Drop the last attachment; the notice URL (last element) stays.
            let drop_idx = parts.len() - 2;
            parts.remove(drop_idx);
            continue;
        }
        // Only the notice URL remains and it still does not fit with the
        // header: truncate the header, never the URL.
        let available_header = UNIFIED_MAX_MESSAGE_LEN.saturating_sub(utf16_len(&footer) + 2);
        let header = truncate_text(header, available_header);
        return format!("{header}\n\n{footer}");
    }
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
/// - Defensively caps length at `UNIFIED_MAX_MESSAGE_LEN` (4,096 UTF-16 code
///   units — the unit Telegram counts, see [`utf16_len`]), preserving header,
///   attachments, and the notice URL while gracefully truncating overflowing
///   body text. The footer is never truncated (M5).
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

    // Construct footer (attachments + notice URL), each on its own line.
    // `footer_parts` keeps the notice URL as the last element so it can
    // always be preserved (M5).
    let mut footer_parts: Vec<String> = valid_attachments.iter().map(|s| s.to_string()).collect();
    footer_parts.push(notice_url.to_string());
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

    if utf16_len(&full_content) <= UNIFIED_MAX_MESSAGE_LEN {
        return full_content;
    }

    // Defensive truncation (M5): truncate body text first, but preserve
    // header, attachments, and notice URL. The footer is never truncated —
    // if there is no body, or no room for one, assemble header + footer
    // directly (dropping attachments / truncating the header only if the
    // header + footer alone still exceed the limit).
    if let Some(body) = text_content {
        let suffix = format!("\n\n{footer}");
        let fixed_overhead = utf16_len(&header) + utf16_len(&suffix) + 2; // "\n\n" between header and body
        let available_body_chars = UNIFIED_MAX_MESSAGE_LEN.saturating_sub(fixed_overhead);
        if available_body_chars >= 10 {
            let truncated_body = truncate_text(body, available_body_chars);
            let truncated = format!("{header}\n\n{truncated_body}{suffix}");
            if utf16_len(&truncated) <= UNIFIED_MAX_MESSAGE_LEN {
                return truncated;
            }
        }
    }
    assemble_header_footer(&header, &footer_parts)
}

/// Backwards-compatible alias for `format_notice_message`.
/// Guarantees that Facebook and future platforms receive the exact same unified visible text.
#[inline]
pub fn format_facebook_message(notice: &Notice, edited: bool) -> String {
    format_notice_message(notice, edited)
}
