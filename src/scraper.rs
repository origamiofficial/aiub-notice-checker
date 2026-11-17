use chrono::NaiveDate;
use futures_util::future::{select, Either};
use std::collections::HashSet;
use std::pin::pin;
use std::time::Duration;
use tl::{Node, Parser, ParserOptions, VDom};
use worker::{
    url::Url, AbortController, Delay, Error, Fetch, Headers, Method, Request, RequestInit,
    RequestRedirect, Result,
};

use crate::config::{
    BASE_HOST, BASE_URL, HTTP_TIMEOUT_MS, LIST_PAGE_SIZE, MAX_HTML_BYTES, MAX_REDIRECTS,
    MAX_URL_LENGTH, NOTICE_PATH,
};
use crate::models::Notice;

const USER_AGENT: &str =
    "Mozilla/5.0 (compatible; AIUBNoticeChecker/0.3; +https://www.aiub.edu)";

pub fn listing_page_url(page_no: usize) -> String {
    format!(
        "{}{}?pageNo={}&pageSize={}",
        BASE_URL, NOTICE_PATH, page_no, LIST_PAGE_SIZE
    )
}

/// Fetches an HTML page from AIUB with a 15-second `AbortController` + `Delay` timeout,
/// manual redirect validation, and response size cap.
pub async fn fetch_html(target_url: &str) -> Result<String> {
    let mut current_url = target_url.to_string();

    for _hop in 0..=MAX_REDIRECTS {
        if current_url.len() > MAX_URL_LENGTH {
            return Err(Error::RustError("URL exceeds maximum allowed length".into()));
        }

        let headers = Headers::new();
        headers.set("User-Agent", USER_AGENT)?;
        headers.set(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )?;
        headers.set("Accept-Language", "en-US,en;q=0.9")?;

        let mut init = RequestInit::new();
        init.with_method(Method::Get)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Manual);

        let req = Request::new_with_init(&current_url, &init)?;
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
                    "HTTP timeout after {}ms fetching {}",
                    HTTP_TIMEOUT_MS, current_url
                )));
            }
        };

        let status = resp.status_code();
        if (300..400).contains(&status) {
            if let Some(loc) = resp.headers().get("Location")? {
                current_url = resolve_url(&current_url, &loc);
                continue;
            }
        }

        if !(200..300).contains(&status) {
            return Err(Error::RustError(format!(
                "HTTP {} when fetching {}",
                status, current_url
            )));
        }

        let text = resp.text().await?;
        if text.len() > MAX_HTML_BYTES {
            return Err(Error::RustError(format!(
                "HTML response too large ({} bytes) for {}",
                text.len(),
                current_url
            )));
        }
        return Ok(text);
    }

    Err(Error::RustError(format!(
        "Too many redirects fetching {}",
        target_url
    )))
}

/// Parses the AIUB notice listing HTML (`ul.event-list li` with fallback to `.notification, .card`).
pub fn parse_listing_html(html: &str) -> Vec<Notice> {
    let Ok(dom) = tl::parse(html, ParserOptions::default()) else {
        return Vec::new();
    };
    let parser = dom.parser();

    let mut item_handles = query_handles(&dom, "ul.event-list li");
    if item_handles.is_empty() {
        item_handles = query_handles(&dom, ".notification, .card");
    }

    let mut notices = Vec::with_capacity(item_handles.len());

    for handle in item_handles {
        let Some(node) = handle.get(parser) else {
            continue;
        };
        let Some(tag) = node.as_tag() else {
            continue;
        };

        let raw_href = find_first_attr_in_tag(tag, parser, "a.notification-link", "href")
            .or_else(|| find_first_attr_in_tag(tag, parser, "a", "href"))
            .unwrap_or_default();

        let url = normalize_notice_url(&raw_href);
        if url.is_empty() || !is_valid_aiub_notice_url(&url) {
            continue;
        }

        let title = find_first_text_in_tag(tag, parser, "h2.title")
            .or_else(|| find_first_text_in_tag(tag, parser, ".title"))
            .or_else(|| find_first_text_in_tag(tag, parser, "h2, h3, h4, a"))
            .map(|s| clean_whitespace(&s))
            .unwrap_or_default();

        if title.is_empty() {
            continue;
        }

        let description = find_first_text_in_tag(tag, parser, "p.desc")
            .or_else(|| find_first_text_in_tag(tag, parser, ".desc"))
            .or_else(|| find_first_text_in_tag(tag, parser, "p"))
            .map(|s| clean_whitespace(&s))
            .unwrap_or_default();

        let published_date = extract_iso_date_from_card(tag, parser);

        notices.push(Notice {
            title,
            description,
            url,
            published_date,
            body_text: String::new(),
            attachments: Vec::new(),
            detail_fetched: false,
        });
    }

    notices
}

/// Parses an AIUB notice detail page without multi-step descendant combinators
/// (`tl 0.7.8` returns `None` on selectors like `div.notice-page div.question-column`).
pub fn parse_detail_html(html: &str, page_url: &str) -> Option<(String, Vec<String>)> {
    let dom = tl::parse(html, ParserOptions::default()).ok()?;
    let parser = dom.parser();

    let container_handle = find_detail_container(&dom, parser)?;
    let node = container_handle.get(parser)?;
    let tag = node.as_tag()?;

    let body_text = extract_clean_text_from_tag(tag, parser);
    let attachments = extract_attachments_from_tag(tag, parser, page_url);

    if body_text.is_empty() && attachments.is_empty() {
        return None;
    }

    Some((body_text, attachments))
}

fn find_detail_container(dom: &VDom, parser: &Parser) -> Option<tl::NodeHandle> {
    if let Some(notice_pages) = dom.query_selector("div.notice-page") {
        for page_handle in notice_pages {
            if let Some(page_node) = page_handle.get(parser) {
                if let Some(page_tag) = page_node.as_tag() {
                    if let Some(mut cols) = page_tag.query_selector(parser, "div.question-column") {
                        if let Some(col_handle) = cols.next() {
                            return Some(col_handle);
                        }
                    }
                    return Some(page_handle);
                }
            }
        }
    }

    for sel in [
        "div.question-column",
        "div.notice-details",
        "div.notice-content",
        "article",
        "main",
    ] {
        if let Some(mut iter) = dom.query_selector(sel) {
            if let Some(first) = iter.next() {
                return Some(first);
            }
        }
    }

    None
}

fn extract_clean_text_from_tag(tag: &tl::HTMLTag, parser: &Parser) -> String {
    let raw_html = tag.inner_html(parser);
    let mut spaced = String::with_capacity(raw_html.len());

    let lower = raw_html.to_ascii_lowercase();
    let bytes = raw_html.as_bytes();
    let lower_bytes = lower.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(close_rel) = bytes[i..].iter().position(|&b| b == b'>') {
                let tag_slice = &lower_bytes[i..=i + close_rel];
                if is_script_or_style_open(tag_slice) {
                    if let Some(end_rel) =
                        find_closing_ignored_tag(&lower_bytes[i + close_rel + 1..])
                    {
                        i = i + close_rel + 1 + end_rel;
                        continue;
                    }
                }
                if is_block_tag(tag_slice) {
                    spaced.push('\n');
                } else {
                    spaced.push(' ');
                }
                i += close_rel + 1;
                continue;
            }
        }
        spaced.push(bytes[i] as char);
        i += 1;
    }

    let decoded = html_escape::decode_html_entities(&spaced);
    let mut lines: Vec<String> = Vec::new();

    for raw_line in decoded.lines() {
        let cleaned = clean_whitespace(raw_line);
        if cleaned.is_empty() {
            continue;
        }
        if cleaned.eq_ignore_ascii_case("Back to Notice List")
            || cleaned.eq_ignore_ascii_case("Notice")
        {
            continue;
        }
        lines.push(cleaned);
    }

    lines.join("\n")
}

fn extract_attachments_from_tag(
    tag: &tl::HTMLTag,
    parser: &Parser,
    page_url: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen_urls = HashSet::new();

    if let Some(anchors) = tag.query_selector(parser, "a") {
        for handle in anchors {
            let Some(node) = handle.get(parser) else {
                continue;
            };
            let Some(a_tag) = node.as_tag() else {
                continue;
            };
            let Some(raw_href) = a_tag
                .attributes()
                .get("href")
                .flatten()
                .map(|b| b.as_utf8_str().to_string())
            else {
                continue;
            };

            let trimmed = raw_href.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.to_ascii_lowercase().starts_with("javascript:")
                || trimmed.to_ascii_lowercase().starts_with("mailto:")
            {
                continue;
            }

            let resolved = resolve_url(page_url, trimmed);
            if resolved == page_url
                || resolved.ends_with(NOTICE_PATH)
                || !seen_urls.insert(resolved.clone())
            {
                continue;
            }

            out.push(resolved);
        }
    }

    out
}

fn query_handles(dom: &VDom, selector: &str) -> Vec<tl::NodeHandle> {
    dom.query_selector(selector)
        .map(|iter| iter.collect())
        .unwrap_or_default()
}

fn find_first_attr_in_tag(
    tag: &tl::HTMLTag,
    parser: &Parser,
    selector: &str,
    attr: &str,
) -> Option<String> {
    let mut iter = tag.query_selector(parser, selector)?;
    while let Some(handle) = iter.next() {
        let node = handle.get(parser)?;
        let inner_tag = node.as_tag()?;
        if let Some(val) = inner_tag.attributes().get(attr).flatten() {
            let s = val.as_utf8_str().trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn find_first_text_in_tag(
    tag: &tl::HTMLTag,
    parser: &Parser,
    selector: &str,
) -> Option<String> {
    let mut iter = tag.query_selector(parser, selector)?;
    while let Some(handle) = iter.next() {
        let node: &Node = handle.get(parser)?;
        let raw = node.inner_text(parser);
        let decoded = html_escape::decode_html_entities(&raw);
        let cleaned = clean_whitespace(&decoded);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

fn extract_iso_date_from_card(tag: &tl::HTMLTag, parser: &Parser) -> Option<String> {
    let day = find_first_text_in_tag(tag, parser, ".day").unwrap_or_default();
    let month = find_first_text_in_tag(tag, parser, ".month").unwrap_or_default();
    let year = find_first_text_in_tag(tag, parser, ".year").unwrap_or_default();

    if !day.is_empty() && !month.is_empty() && !year.is_empty() {
        let combined = clean_whitespace(&format!("{day} {month} {year}"));
        if let Some(iso) = parse_date_to_iso(&combined) {
            return Some(iso);
        }
    }

    if let Some(raw_time) = find_first_text_in_tag(tag, parser, "time")
        .or_else(|| find_first_text_in_tag(tag, parser, ".date"))
    {
        return parse_date_to_iso(&raw_time);
    }

    None
}

fn parse_date_to_iso(raw: &str) -> Option<String> {
    let cleaned = clean_whitespace(raw).replace(',', "");
    for fmt in ["%d %b %Y", "%d %B %Y", "%Y-%m-%d", "%d-%m-%Y", "%d/%m/%Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(&cleaned, fmt) {
            return Some(d.format("%Y-%m-%d").to_string());
        }
    }
    None
}

pub fn normalize_notice_url(raw_href: &str) -> String {
    let trimmed = raw_href.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.to_ascii_lowercase().starts_with("javascript:")
    {
        return String::new();
    }

    let resolved = resolve_url(BASE_URL, trimmed);
    if let Ok(mut parsed) = Url::parse(&resolved) {
        parsed.set_fragment(None);
        let mut s = parsed.to_string();
        if s.ends_with('/') && parsed.path() != "/" {
            s.pop();
        }
        s
    } else {
        resolved
    }
}

fn is_valid_aiub_notice_url(url: &str) -> bool {
    if url.len() > MAX_URL_LENGTH {
        return false;
    }
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if parsed.host_str() != Some(BASE_HOST) {
        return false;
    }
    let path = parsed.path().trim_matches('/');
    !path.is_empty() && parsed.path() != NOTICE_PATH
}

fn resolve_url(base: &str, href: &str) -> String {
    if let Ok(base_url) = Url::parse(base) {
        if let Ok(joined) = base_url.join(href) {
            return joined.to_string();
        }
    }
    href.to_string()
}

fn clean_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_script_or_style_open(tag_slice: &[u8]) -> bool {
    tag_slice.starts_with(b"<script") || tag_slice.starts_with(b"<style")
}

fn find_closing_ignored_tag(haystack: &[u8]) -> Option<usize> {
    for target in [b"</script>".as_slice(), b"</style>".as_slice()] {
        if let Some(pos) = haystack.windows(target.len()).position(|w| w == target) {
            return Some(pos + target.len());
        }
    }
    None
}

fn is_block_tag(tag_slice: &[u8]) -> bool {
    for prefix in [
        b"<p".as_slice(),
        b"</p>".as_slice(),
        b"<br".as_slice(),
        b"<div".as_slice(),
        b"</div>".as_slice(),
        b"<li".as_slice(),
        b"</li>".as_slice(),
        b"<tr".as_slice(),
        b"</tr>".as_slice(),
        b"<h1".as_slice(),
        b"<h2".as_slice(),
        b"<h3".as_slice(),
        b"<h4".as_slice(),
    ] {
        if tag_slice.starts_with(prefix) {
            return true;
        }
    }
    false
}