use chrono::NaiveDate;
use futures_util::future::{Either, select};
use std::collections::HashSet;
use std::pin::{Pin, pin};
use std::time::Duration;
use tl::{HTMLTag, Node, Parser, ParserOptions, VDom};
use worker::{
    AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Result,
    console_warn, url::Url,
};

use crate::config::{
    BASE_HOST, BASE_URL, HTTP_TIMEOUT_MS, LIST_PAGE_SIZE, MAX_HTML_BYTES, MAX_REDIRECTS,
    MAX_URL_LENGTH, NOTICE_PATH,
};
use crate::errors::AppError;
use crate::models::Notice;

pub(crate) const USER_AGENT: &str = concat!(
    "aiub-notice-checker/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/origamiofficial/aiub-notice-checker)"
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDetail {
    pub title: Option<String>,
    pub published_date: Option<String>,
    pub body_text: String,
    pub attachments: Vec<String>,
    /// Text extracted from the first `div.question-column.notice-sticky-header` so existing
    /// D1 rows populated by version `e1a14563` can be upgraded silently without false `[EDITED]` alerts.
    pub legacy_sticky_body: String,
}

/// Represents the granular outcome of attempting to fetch and parse an AIUB notice detail page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailFetchOutcome {
    /// HTTP 200 OK and DOM parser successfully extracted notice content.
    Success(ParsedDetail),
    /// HTTP error, timeout, network failure, or non-2xx status code.
    HttpError(String),
    /// HTTP 200 OK received, but DOM parsing returned empty or missing selectors.
    ParseError {
        reason: &'static str,
        html_len: usize,
    },
    /// Deferred before fetch because the Cloudflare Worker approached its CPU budget.
    CpuSkipped,
}

#[must_use]
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
            return Err(AppError::Fetch("URL exceeds maximum allowed length".into()).into());
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
        // Medium #5: ONE shared 15s deadline covers `send()` AND the
        // body read — a fresh Delay per phase previously allowed one
        // fetch to take up to 30s. `select` returns the losing future,
        // so the same Delay is reused for the body read below.
        // `timeout` is a named local (not `pin!(...)` inline): the
        // Pin borrowing it is returned by `select` and reused for the
        // body read below, so it must outlive this statement.
        let mut timeout = Delay::from(Duration::from_millis(HTTP_TIMEOUT_MS));

        let (mut resp, timeout_fut) = match select(pin!(send_fut), Pin::new(&mut timeout)).await {
            Either::Left((res, timeout_fut)) => match res {
                Ok(r) => (r, timeout_fut),
                Err(e) => return Err(AppError::Fetch(e.to_string()).into()),
            },
            Either::Right(((), _)) => {
                controller.abort();
                return Err(AppError::Fetch(format!(
                    "HTTP timeout after {HTTP_TIMEOUT_MS}ms fetching {current_url}"
                ))
                .into());
            }
        };

        let status = resp.status_code();
        if (300..400).contains(&status)
            && let Some(loc) = resp.headers().get("Location")?
        {
            current_url = normalize_aiub_url(&loc, &current_url, false);
            if current_url.is_empty() {
                return Err(
                    AppError::Fetch(format!("Unsafe redirect location from {target_url}")).into(),
                );
            }
            continue;
        }

        if !(200..300).contains(&status) {
            return Err(
                AppError::Fetch(format!("HTTP {status} when fetching {current_url}")).into(),
            );
        }

        // M9: check Content-Length before buffering the body — a lying
        // or huge response must be rejected without downloading it first.
        if let Ok(Some(len_str)) = resp.headers().get("Content-Length")
            && let Ok(len) = len_str.parse::<usize>()
            && len > MAX_HTML_BYTES
        {
            return Err(AppError::Fetch(format!(
                "HTML response too large (Content-Length {len} bytes) for {current_url}"
            ))
            .into());
        }

        // M9: the 15s timeout must cover the body read too, not just
        // `send()`. A stalled body stream previously hung the run past
        // the timeout (and the 2 MB cap only fired after full buffering).
        // The deadline is the same one started above, so send + body
        // share 15s.
        let text_fut = resp.text();
        let text = match select(pin!(text_fut), timeout_fut).await {
            Either::Left((res, _)) => res?,
            Either::Right(((), _)) => {
                controller.abort();
                return Err(AppError::Fetch(format!(
                    "HTTP timeout after {HTTP_TIMEOUT_MS}ms reading body of {current_url}"
                ))
                .into());
            }
        };
        if text.len() > MAX_HTML_BYTES {
            return Err(AppError::Fetch(format!(
                "HTML response too large ({} bytes) for {current_url}",
                text.len()
            ))
            .into());
        }
        return Ok(text);
    }

    Err(AppError::Fetch(format!("Too many redirects fetching {target_url}")).into())
}

/// Parses the AIUB notice listing page matching `main.py` (`div.notification`, `h2.title`,
/// `a.info-link`, `p.desc`, and `div.date-custom`).
#[must_use]
pub fn parse_listing_html(html: &str) -> Vec<Notice> {
    let Ok(dom) = tl::parse(html, ParserOptions::default()) else {
        return Vec::new();
    };
    let parser = dom.parser();

    let mut item_handles = query_handles(&dom, "div.notification");
    if item_handles.is_empty() {
        item_handles = find_listing_li_handles(&dom, parser);
    }
    // M16: the `.card` fallback is scoped to real notice cards only. A
    // bare `.card` previously matched any card on the page (promo tiles,
    // event cards, sidebar widgets) and the generic `a[href]` fallback
    // below then harvested whatever link it contained, misparsing
    // unrelated cards as notices. A card only counts when it carries an
    // `a.info-link` / `a.notification-link` — the selectors real AIUB
    // notice cards use.
    let mut from_card_fallback = false;
    if item_handles.is_empty() {
        item_handles = query_handles(&dom, ".card")
            .into_iter()
            .filter(|handle| {
                let Some(node) = handle.get(parser) else {
                    return false;
                };
                let Some(tag) = node.as_tag() else {
                    return false;
                };
                find_first_attr_in_tag(tag, parser, "a.info-link", "href").is_some()
                    || find_first_attr_in_tag(tag, parser, "a.notification-link", "href").is_some()
            })
            .collect();
        from_card_fallback = true;
    }

    let mut notices = Vec::with_capacity(item_handles.len());

    for handle in item_handles {
        let Some(node) = handle.get(parser) else {
            continue;
        };
        let Some(tag) = node.as_tag() else {
            continue;
        };

        // O4: one subtree walk per card instead of ~15 selector walks.
        let fields = extract_listing_fields(tag, parser, from_card_fallback);

        let url = normalize_notice_url(&fields.raw_href);
        if url.is_empty() || !is_valid_aiub_notice_url(&url) {
            continue;
        }

        if fields.title.is_empty() {
            continue;
        }

        notices.push(Notice {
            title: fields.title,
            description: fields.description,
            url,
            published_date: fields.published_date,
            body_text: String::new(),
            attachments: Vec::new(),
            detail_fetched: false,
        });
    }

    notices
}

/// O4: the fields of one listing card, collected by a single subtree
/// walk (see `extract_listing_fields`).
struct ListingFields {
    raw_href: String,
    title: String,
    description: String,
    published_date: Option<String>,
}

/// O4: single-pass listing extraction. The old code ran ~15
/// `query_selector` subtree walks and 4–5 full-text copies per card
/// (href ×3, title ×5, description ×3, date ×6) — roughly 7,500
/// traversals for a 500-card page, 750 for a normal 50-notice page.
/// This walks each card's subtree once, in document order (the same
/// order `query_selector` visits), tracking the best (lowest-priority)
/// candidate for each field and extracting text only for candidates
/// that can win — matching the old per-selector first-non-empty
/// semantics exactly. Measured on synthetic 500-card pages the parse
/// is dominated by `tl::parse` tokenization, so this is roughly
/// performance-neutral there (8 ms vs 8 ms median heavy, 3–5 ms vs
/// 3 ms light); the traversal reduction is real, the ms saving at
/// this scale is not. The O2 parse timer is now the tool to measure
/// it on real AIUB HTML.
///
/// Selector priority (lower wins, ties keep the first in document
/// order): href `a.info-link` > `a.notification-link` > `a`; title
/// `h2.title` > `.title` > `h2` > `h3` > `h4`; description `p.desc` >
/// `.desc` > `p`; date `.date-custom` > `.day`/`.month`/`.year` >
/// `time[datetime]` > `time` / `.date`.
fn extract_listing_fields(
    tag: &HTMLTag,
    parser: &Parser,
    from_card_fallback: bool,
) -> ListingFields {
    let mut state = CardWalkState::default();
    let card_children = tag.children();
    for handle in card_children.top().iter() {
        visit_card_node(*handle, parser, from_card_fallback, 0, &mut state);
    }

    // Date resolution, same order as `extract_iso_date_from_card`.
    let mut published_date = None;
    if let Some(raw_custom) = &state.date_custom {
        published_date = parse_date_to_iso(raw_custom);
    }
    if published_date.is_none()
        && let (Some(d), Some(m), Some(y)) = (&state.day, &state.month, &state.year)
    {
        let combined = clean_whitespace(&format!("{d} {m} {y}"));
        published_date = parse_date_to_iso(&combined);
    }
    if published_date.is_none()
        && let Some(dt_attr) = &state.time_datetime
    {
        let prefix: String = dt_attr.chars().take(10).collect();
        published_date = parse_date_to_iso(&prefix).or_else(|| parse_date_to_iso(dt_attr));
    }
    if published_date.is_none() {
        let raw_time = state.time_text.as_ref().or(state.date_text.as_ref());
        if let Some(raw) = raw_time {
            published_date = parse_date_to_iso(raw);
        }
    }

    ListingFields {
        raw_href: state.raw_href,
        title: state.title,
        description: state.description,
        published_date,
    }
}

/// O4: the best-so-far candidates found by the single card walk.
struct CardWalkState {
    href_priority: u8,
    raw_href: String,
    title_priority: u8,
    title: String,
    desc_priority: u8,
    description: String,
    date_custom: Option<String>,
    day: Option<String>,
    month: Option<String>,
    year: Option<String>,
    time_datetime: Option<String>,
    time_text: Option<String>,
    date_text: Option<String>,
}

impl Default for CardWalkState {
    fn default() -> Self {
        Self {
            href_priority: u8::MAX,
            raw_href: String::new(),
            title_priority: u8::MAX,
            title: String::new(),
            desc_priority: u8::MAX,
            description: String::new(),
            date_custom: None,
            day: None,
            month: None,
            year: None,
            time_datetime: None,
            time_text: None,
            date_text: None,
        }
    }
}

/// O4: one step of the single-pass card walk — classifies one element,
/// then recurses into its children in document order (preorder DFS,
/// matching query_selector's order). Text is extracted only for a
/// candidate that can win (strictly better priority than the current
/// best), so a card costs one traversal plus a handful of text
/// extractions instead of ~15 subtree walks.
///
/// `depth` is capped at `MAX_CARD_WALK_DEPTH`: the listing HTML comes
/// from the network, and an adversarially nested page must not grow
/// the recursion without bound. Real AIUB cards nest well under 20
/// deep; deeper subtrees simply contribute no fields.
const MAX_CARD_WALK_DEPTH: usize = 64;

fn visit_card_node(
    handle: tl::NodeHandle,
    parser: &Parser,
    from_card_fallback: bool,
    depth: usize,
    state: &mut CardWalkState,
) {
    if depth > MAX_CARD_WALK_DEPTH {
        return;
    }
    let Some(node) = handle.get(parser) else {
        return;
    };
    let Some(el) = node.as_tag() else {
        return;
    };
    let name = el.name().as_utf8_str();
    let name = name.as_ref();
    // O4: split the class attribute once per element instead of once
    // per has_class() call (there are ~8 class checks below). The
    // first version re-split it every time and measured slower than
    // the old 15-walk code.
    let class_attr = el.attributes().class().map(|cls| cls.as_utf8_str());
    let classes: Vec<&str> = class_attr
        .as_ref()
        .map(|s| s.split_whitespace().collect())
        .unwrap_or_default();
    let has = |target: &str| classes.iter().any(|c| c.eq_ignore_ascii_case(target));

    if name.eq_ignore_ascii_case("a") {
        let prio = if has("info-link") {
            0
        } else if has("notification-link") {
            1
        } else {
            2
        };
        // M16: inside a `.card` fallback item, a generic `a[href]`
        // never counts — that is how unrelated links inside a card
        // were harvested as notice URLs.
        if prio < state.href_priority
            && !(from_card_fallback && prio == 2)
            && let Some(href) = get_attr_str(el, "href")
        {
            state.href_priority = prio;
            state.raw_href = href;
        }
    }

    let title_prio = if name.eq_ignore_ascii_case("h2") && has("title") {
        0
    } else if has("title") {
        1
    } else if name.eq_ignore_ascii_case("h2") {
        2
    } else if name.eq_ignore_ascii_case("h3") {
        3
    } else if name.eq_ignore_ascii_case("h4") {
        4
    } else {
        u8::MAX
    };
    if title_prio < state.title_priority {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.title_priority = title_prio;
            state.title = text;
        }
    }

    let desc_prio = if name.eq_ignore_ascii_case("p") && has("desc") {
        0
    } else if has("desc") {
        1
    } else if name.eq_ignore_ascii_case("p") {
        2
    } else {
        u8::MAX
    };
    if desc_prio < state.desc_priority {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.desc_priority = desc_prio;
            state.description = text;
        }
    }

    if state.date_custom.is_none() && has("date-custom") {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.date_custom = Some(text);
        }
    }
    if state.day.is_none() && has("day") {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.day = Some(text);
        }
    }
    if state.month.is_none() && has("month") {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.month = Some(text);
        }
    }
    if state.year.is_none() && has("year") {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.year = Some(text);
        }
    }
    if name.eq_ignore_ascii_case("time") {
        if state.time_datetime.is_none()
            && let Some(dt) = get_attr_str(el, "datetime")
        {
            state.time_datetime = Some(dt);
        }
        if state.time_text.is_none() {
            let text = extract_spaced_inline_text(el, parser);
            if !text.is_empty() {
                state.time_text = Some(text);
            }
        }
    }
    if state.date_text.is_none() && has("date") {
        let text = extract_spaced_inline_text(el, parser);
        if !text.is_empty() {
            state.date_text = Some(text);
        }
    }

    let children = el.children();
    for child in children.top().iter() {
        visit_card_node(*child, parser, from_card_fallback, depth + 1, state);
    }
}

fn find_listing_li_handles(dom: &VDom, parser: &Parser) -> Vec<tl::NodeHandle> {
    let mut out = Vec::new();
    if let Some(lists) = dom.query_selector("ul.event-list") {
        for ul_handle in lists {
            if let Some(ul_node) = ul_handle.get(parser)
                && let Some(ul_tag) = ul_node.as_tag()
                && let Some(items) = ul_tag.query_selector(parser, "li")
            {
                out.extend(items);
            }
        }
    }
    out
}

/// Parses an AIUB notice detail page matching `main.py`:
/// - Full title from `h1#dynamicHeading`
/// - Body text and attachments (`a[href]`, `img[src]`, `img[data-src]`) from all
///   `div.question-column` inside `div.notice-page` that do NOT have class `notice-sticky-header`.
#[must_use]
pub fn parse_detail_html(html: &str, page_url: &str) -> Option<ParsedDetail> {
    parse_detail_with_reason(html, page_url).ok()
}

/// Parses an AIUB notice detail page, returning a specific error reason if parsing/validation fails.
pub fn parse_detail_with_reason(html: &str, page_url: &str) -> Result<ParsedDetail, &'static str> {
    if html.trim().is_empty() {
        return Err("HTML content is empty");
    }

    let Ok(dom) = tl::parse(html, ParserOptions::default()) else {
        return Err("HTML tokenization failed");
    };
    let parser = dom.parser();

    // 1. Extract full untruncated title from `h1#dynamicHeading` (`DETAIL_TITLE_XPATH` in `main.py`).
    let detail_title = dom
        .get_element_by_id("dynamicHeading")
        .and_then(|h| h.get(parser))
        .and_then(|n| n.as_tag())
        .map(|t| extract_spaced_inline_text(t, parser))
        .filter(|s| !s.is_empty())
        .or_else(|| {
            dom.query_selector("h1#dynamicHeading")
                .and_then(|mut it| it.next())
                .and_then(|h| h.get(parser))
                .and_then(|n| n.as_tag())
                .map(|t| extract_spaced_inline_text(t, parser))
                .filter(|s| !s.is_empty())
        });

    // 2. Locate all non-sticky `div.question-column` containers inside `div.notice-page`
    //    (and capture the sticky header for fallback date + legacy re-baseline).
    let mut body_handles: Vec<tl::NodeHandle> = Vec::new();
    let mut sticky_handle: Option<tl::NodeHandle> = None;

    if let Some(notice_pages) = dom.query_selector("div.notice-page") {
        for page_handle in notice_pages {
            let Some(page_node) = page_handle.get(parser) else {
                continue;
            };
            let Some(page_tag) = page_node.as_tag() else {
                continue;
            };
            if let Some(cols) = page_tag.query_selector(parser, "div.question-column") {
                for col_handle in cols {
                    let Some(col_node) = col_handle.get(parser) else {
                        continue;
                    };
                    let Some(col_tag) = col_node.as_tag() else {
                        continue;
                    };
                    if has_class(col_tag, "notice-sticky-header") {
                        if sticky_handle.is_none() {
                            sticky_handle = Some(col_handle);
                        }
                    } else {
                        body_handles.push(col_handle);
                    }
                }
            }
        }
    }

    if body_handles.is_empty()
        && let Some(cols) = dom.query_selector("div.question-column")
    {
        for col_handle in cols {
            let Some(col_node) = col_handle.get(parser) else {
                continue;
            };
            let Some(col_tag) = col_node.as_tag() else {
                continue;
            };
            if has_class(col_tag, "notice-sticky-header") {
                if sticky_handle.is_none() {
                    sticky_handle = Some(col_handle);
                }
            } else {
                body_handles.push(col_handle);
            }
        }
    }

    if body_handles.is_empty() {
        return Err("missing non-sticky question-column container");
    }

    let mut body_parts: Vec<String> = Vec::new();
    let mut attachments: Vec<String> = Vec::new();
    let mut seen_attachments: HashSet<String> = HashSet::new();

    for handle in &body_handles {
        let Some(node) = handle.get(parser) else {
            continue;
        };
        let Some(tag) = node.as_tag() else {
            continue;
        };

        let part_text = extract_visible_detail_text(tag, parser);
        if !part_text.is_empty() {
            body_parts.push(part_text);
        }

        extract_attachments_from_tag(
            tag,
            parser,
            page_url,
            &mut attachments,
            &mut seen_attachments,
        );
    }

    let body_text = body_parts.join("\n\n");
    if body_text.is_empty() && attachments.is_empty() {
        return Err("question-column contained no text or attachments");
    }

    let mut detail_date = None;
    let mut legacy_sticky_body = String::new();
    if let Some(sh) = sticky_handle
        && let Some(sn) = sh.get(parser)
        && let Some(st) = sn.as_tag()
    {
        detail_date = extract_iso_date_from_card(st, parser);
        legacy_sticky_body = extract_legacy_multiline_text(st, parser);
    }

    Ok(ParsedDetail {
        title: detail_title,
        published_date: detail_date,
        body_text,
        attachments,
        legacy_sticky_body,
    })
}

fn has_class(tag: &HTMLTag, target_class: &str) -> bool {
    tag.attributes()
        .class()
        .map(|cls| {
            cls.as_utf8_str()
                .split_whitespace()
                .any(|c| c.eq_ignore_ascii_case(target_class))
        })
        .unwrap_or(false)
}

/// Matches `main.py`'s `clean_text(" ".join(node.xpath(VISIBLE_DETAIL_TEXT_XPATH)))`,
/// ignoring `<script>`, `<style>`, `<noscript>`, and `<template>` subtrees while preserving UTF-8.
fn extract_visible_detail_text(tag: &HTMLTag, parser: &Parser) -> String {
    let raw_html = tag.inner_html(parser);
    let stripped = strip_tags_with_spaces(&raw_html, true);
    let decoded = html_escape::decode_html_entities(&stripped);
    clean_whitespace(&decoded)
}

fn extract_spaced_inline_text(tag: &HTMLTag, parser: &Parser) -> String {
    let raw_html = tag.inner_html(parser);
    let stripped = strip_tags_with_spaces(&raw_html, true);
    let decoded = html_escape::decode_html_entities(&stripped);
    clean_whitespace(&decoded)
}

/// Preserves valid UTF-8 text slices between HTML tags while inserting newlines for block tags.
fn extract_legacy_multiline_text(tag: &HTMLTag, parser: &Parser) -> String {
    let raw_html = tag.inner_html(parser);
    let mut spaced = String::with_capacity(raw_html.len());
    let bytes = raw_html.as_bytes();
    let mut i = 0;
    let mut text_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(close_rel) = bytes[i..].iter().position(|&b| b == b'>')
        {
            if text_start < i {
                spaced.push_str(&raw_html[text_start..i]);
            }
            let tag_slice = &bytes[i..=i + close_rel];
            if let Some(closing_tag) = matching_ignored_closing_tag(tag_slice)
                && let Some(end_rel) =
                    find_closing_tag_case_insensitive(&bytes[i + close_rel + 1..], closing_tag)
            {
                i = i + close_rel + 1 + end_rel;
                text_start = i;
                continue;
            }
            if is_block_tag(tag_slice) {
                spaced.push('\n');
            } else {
                spaced.push(' ');
            }
            i += close_rel + 1;
            text_start = i;
            continue;
        }
        i += 1;
    }

    if text_start < bytes.len() {
        spaced.push_str(&raw_html[text_start..]);
    }

    let decoded = html_escape::decode_html_entities(&spaced);
    let mut lines: Vec<String> = Vec::new();
    for raw_line in decoded.lines() {
        let cleaned = clean_whitespace(raw_line);
        if cleaned.is_empty()
            || cleaned.eq_ignore_ascii_case("Back to Notice List")
            || cleaned.eq_ignore_ascii_case("Notice")
        {
            continue;
        }
        lines.push(cleaned);
    }
    lines.join("\n")
}

/// Strips HTML tags by slicing `raw_html` on ASCII `<` and `>` boundaries and pushing
/// valid UTF-8 `&str` slices (`&raw_html[text_start..i]`), never casting individual bytes to `char`.
fn strip_tags_with_spaces(raw_html: &str, skip_ignored_blocks: bool) -> String {
    let mut out = String::with_capacity(raw_html.len());
    let bytes = raw_html.as_bytes();
    let mut i = 0;
    let mut text_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(close_rel) = bytes[i..].iter().position(|&b| b == b'>')
        {
            if text_start < i {
                out.push_str(&raw_html[text_start..i]);
            }
            let tag_slice = &bytes[i..=i + close_rel];
            if skip_ignored_blocks
                && let Some(closing_tag) = matching_ignored_closing_tag(tag_slice)
                && let Some(end_rel) =
                    find_closing_tag_case_insensitive(&bytes[i + close_rel + 1..], closing_tag)
            {
                i = i + close_rel + 1 + end_rel;
                text_start = i;
                continue;
            }
            out.push(' ');
            i += close_rel + 1;
            text_start = i;
            continue;
        }
        i += 1;
    }

    if text_start < bytes.len() {
        out.push_str(&raw_html[text_start..]);
    }

    out
}

/// Extracts attachments matching `main.py`: `.//a[@href]/@href | .//img[@src]/@src | .//img[@data-src]/@data-src`
/// with `allow_external = true` and excluding `BASE_URL` and `page_url`.
fn extract_attachments_from_tag(
    tag: &HTMLTag,
    parser: &Parser,
    page_url: &str,
    out: &mut Vec<String>,
    seen_urls: &mut HashSet<String>,
) {
    let base_trimmed = BASE_URL.trim_end_matches('/');
    let page_trimmed = page_url.trim_end_matches('/');

    if let Some(anchors) = tag.query_selector(parser, "a") {
        for handle in anchors {
            let Some(a_tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            if let Some(raw_href) = get_attr_str(a_tag, "href") {
                push_normalized_attachment(
                    &raw_href,
                    page_url,
                    base_trimmed,
                    page_trimmed,
                    out,
                    seen_urls,
                );
            }
        }
    }

    if let Some(images) = tag.query_selector(parser, "img") {
        for handle in images {
            let Some(img_tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            for attr in ["src", "data-src"] {
                if let Some(raw_src) = get_attr_str(img_tag, attr) {
                    push_normalized_attachment(
                        &raw_src,
                        page_url,
                        base_trimmed,
                        page_trimmed,
                        out,
                        seen_urls,
                    );
                }
            }
        }
    }
}

fn push_normalized_attachment(
    raw_href: &str,
    page_url: &str,
    base_trimmed: &str,
    page_trimmed: &str,
    out: &mut Vec<String>,
    seen_urls: &mut HashSet<String>,
) {
    let trimmed = raw_href.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return;
    }
    let normalized = normalize_aiub_url(trimmed, page_url, true);
    if normalized.is_empty() {
        return;
    }
    let norm_trimmed = normalized.trim_end_matches('/');
    if norm_trimmed == base_trimmed || norm_trimmed == page_trimmed {
        return;
    }
    if seen_urls.insert(normalized.clone()) {
        out.push(normalized);
    }
}

fn get_attr_str(tag: &HTMLTag, attr: &str) -> Option<String> {
    tag.attributes()
        .get(attr)
        .flatten()
        .map(|b| b.as_utf8_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn query_handles(dom: &VDom, selector: &str) -> Vec<tl::NodeHandle> {
    dom.query_selector(selector)
        .map(|iter| iter.collect())
        .unwrap_or_default()
}

fn find_first_attr_in_tag(
    tag: &HTMLTag,
    parser: &Parser,
    selector: &str,
    attr: &str,
) -> Option<String> {
    let iter = tag.query_selector(parser, selector)?;
    for handle in iter {
        let node = handle.get(parser)?;
        let inner_tag = node.as_tag()?;
        if let Some(val) = get_attr_str(inner_tag, attr) {
            return Some(val);
        }
    }
    None
}

fn find_first_text_in_tag(tag: &HTMLTag, parser: &Parser, selector: &str) -> Option<String> {
    let iter = tag.query_selector(parser, selector)?;
    for handle in iter {
        let node: &Node = handle.get(parser)?;
        let raw = if let Some(inner_tag) = node.as_tag() {
            extract_spaced_inline_text(inner_tag, parser)
        } else {
            let inner = node.inner_text(parser);
            let decoded = html_escape::decode_html_entities(&inner);
            clean_whitespace(&decoded)
        };
        if !raw.is_empty() {
            return Some(raw);
        }
    }
    None
}

fn extract_iso_date_from_card(tag: &HTMLTag, parser: &Parser) -> Option<String> {
    // 1. Primary AIUB date selector from `main.py`: `div.date-custom` (`.date-custom`)
    if let Some(raw_custom) = find_first_text_in_tag(tag, parser, ".date-custom")
        .or_else(|| find_first_text_in_tag(tag, parser, "div.date-custom"))
        && let Some(iso) = parse_date_to_iso(&raw_custom)
    {
        return Some(iso);
    }

    // 2. Fallback: `.day`, `.month`, `.year`
    let day = find_first_text_in_tag(tag, parser, ".day").unwrap_or_default();
    let month = find_first_text_in_tag(tag, parser, ".month").unwrap_or_default();
    let year = find_first_text_in_tag(tag, parser, ".year").unwrap_or_default();
    if !day.is_empty() && !month.is_empty() && !year.is_empty() {
        let combined = clean_whitespace(&format!("{day} {month} {year}"));
        if let Some(iso) = parse_date_to_iso(&combined) {
            return Some(iso);
        }
    }

    // 3. Fallback: `<time datetime="...">`, `<time>`, or `.date`
    if let Some(dt_attr) = find_first_attr_in_tag(tag, parser, "time", "datetime") {
        let prefix: String = dt_attr.chars().take(10).collect();
        if let Some(iso) = parse_date_to_iso(&prefix).or_else(|| parse_date_to_iso(&dt_attr)) {
            return Some(iso);
        }
    }

    if let Some(raw_time) = find_first_text_in_tag(tag, parser, "time")
        .or_else(|| find_first_text_in_tag(tag, parser, ".date"))
        && let Some(iso) = parse_date_to_iso(&raw_time)
    {
        return Some(iso);
    }

    None
}

fn parse_date_to_iso(raw: &str) -> Option<String> {
    let cleaned = clean_whitespace(raw).replace(',', "");
    if cleaned.is_empty() {
        return None;
    }

    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if tokens.len() >= 3 {
        for window in tokens.windows(3) {
            let day_opt1 = strip_ordinal(window[0]).parse::<u32>();
            let month_opt1 = month_number(window[1]);
            let year_opt1 = window[2].parse::<i32>();
            if let (Ok(day), Some(month), Ok(year)) = (day_opt1, month_opt1, year_opt1)
                && let Some(d) = NaiveDate::from_ymd_opt(year, month, day)
            {
                return Some(d.format("%Y-%m-%d").to_string());
            }

            let month_opt2 = month_number(window[0]);
            let day_opt2 = strip_ordinal(window[1]).parse::<u32>();
            let year_opt2 = window[2].parse::<i32>();
            if let (Some(month), Ok(day), Ok(year)) = (month_opt2, day_opt2, year_opt2)
                && let Some(d) = NaiveDate::from_ymd_opt(year, month, day)
            {
                return Some(d.format("%Y-%m-%d").to_string());
            }
        }
    }

    for fmt in [
        "%Y-%m-%d", "%d %b %Y", "%d %B %Y", "%b %d %Y", "%B %d %Y", "%d-%m-%Y", "%d/%m/%Y",
        "%d-%b-%Y", "%d-%B-%Y",
    ] {
        if let Ok(d) = NaiveDate::parse_from_str(&cleaned, fmt) {
            return Some(d.format("%Y-%m-%d").to_string());
        }
    }

    None
}

fn strip_ordinal(s: &str) -> &str {
    for suffix in ["st", "nd", "rd", "th", "ST", "ND", "RD", "TH"] {
        if let Some(stripped) = s.strip_suffix(suffix)
            && !stripped.is_empty()
            && stripped.chars().all(|c| c.is_ascii_digit())
        {
            return stripped;
        }
    }
    s
}

fn month_number(s: &str) -> Option<u32> {
    let lower = s.trim_matches('.').to_ascii_lowercase();
    match lower.as_str() {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

#[must_use]
pub fn normalize_notice_url(raw_href: &str) -> String {
    let base_with_slash = format!("{BASE_URL}/");
    normalize_aiub_url(raw_href, &base_with_slash, false)
}

#[must_use]
pub fn normalize_aiub_url(raw_href: &str, base_url: &str, allow_external: bool) -> String {
    // O10: only allocate a cleaned copy when a backslash is actually
    // present — the old `replace()` allocated on every href.
    let cleaned: std::borrow::Cow<'_, str> = if raw_href.contains('\\') {
        raw_href.replace('\\', "/").into()
    } else {
        raw_href.into()
    };
    let trimmed = cleaned.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.to_ascii_lowercase().starts_with("javascript:")
        || trimmed.to_ascii_lowercase().starts_with("mailto:")
    {
        return String::new();
    }

    let Ok(base) = Url::parse(base_url) else {
        return String::new();
    };
    let Ok(mut parsed) = base.join(trimmed) else {
        return String::new();
    };

    parsed.set_fragment(None);

    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return String::new();
    }

    let Some(host) = parsed.host_str().map(|h| h.to_ascii_lowercase()) else {
        return String::new();
    };

    if host == BASE_HOST {
        if scheme == "http" {
            // Medium #4: `set_scheme("https")` on an already-parsed
            // http URL cannot realistically fail, but a failure must
            // be visible — the old `let _ =` hid it. The URL is still
            // returned (http AIUB URLs are usable); only the upgrade
            // is skipped.
            if let Err(err) = parsed.set_scheme("https") {
                console_warn!("Failed to upgrade AIUB URL scheme to https ({parsed}): {err:?}");
            }
        }
    } else if !allow_external {
        return String::new();
    }

    let mut out = parsed.to_string();
    if out.len() > MAX_URL_LENGTH {
        return String::new();
    }
    if host == BASE_HOST && out.ends_with('/') && parsed.path() != "/" {
        out.pop();
    }
    out
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

#[inline]
pub(crate) fn is_invisible_control_char(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}')
}

pub(crate) fn clean_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_whitespace = true; // trims leading whitespace

    for c in input.chars() {
        if is_invisible_control_char(c) {
            continue;
        }
        if c.is_whitespace() {
            if !in_whitespace {
                out.push(' ');
                in_whitespace = true;
            }
        } else {
            out.push(c);
            in_whitespace = false;
        }
    }

    if in_whitespace && !out.is_empty() {
        out.pop(); // remove trailing space if one was pushed
    }

    out
}

fn matching_ignored_closing_tag(open_tag_slice: &[u8]) -> Option<&'static [u8]> {
    let stripped = open_tag_slice.strip_prefix(b"<")?;
    let name_end = stripped
        .iter()
        .position(|&b| b == b'>' || b == b'/' || b.is_ascii_whitespace())
        .unwrap_or(stripped.len());
    let name = &stripped[..name_end];

    if name.eq_ignore_ascii_case(b"script") {
        Some(b"</script>")
    } else if name.eq_ignore_ascii_case(b"style") {
        Some(b"</style>")
    } else if name.eq_ignore_ascii_case(b"noscript") {
        Some(b"</noscript>")
    } else if name.eq_ignore_ascii_case(b"template") {
        Some(b"</template>")
    } else {
        None
    }
}

fn find_closing_tag_case_insensitive(haystack: &[u8], closing_tag: &[u8]) -> Option<usize> {
    if haystack.len() < closing_tag.len() {
        return None;
    }
    haystack
        .windows(closing_tag.len())
        .position(|w| {
            w.iter()
                .zip(closing_tag.iter())
                .all(|(&a, &b)| a.to_ascii_lowercase() == b)
        })
        .map(|pos| pos + closing_tag.len())
}

pub(crate) fn is_block_tag(tag_slice: &[u8]) -> bool {
    let stripped = if let Some(s) = tag_slice.strip_prefix(b"</") {
        s
    } else if let Some(s) = tag_slice.strip_prefix(b"<") {
        s
    } else {
        return false;
    };

    let name_end = stripped
        .iter()
        .position(|&b| b == b'>' || b == b'/' || b.is_ascii_whitespace())
        .unwrap_or(stripped.len());
    let name = &stripped[..name_end];

    for block_name in [
        b"p".as_slice(),
        b"br".as_slice(),
        b"div".as_slice(),
        b"li".as_slice(),
        b"tr".as_slice(),
        // L1: <pre> was matched by the old prefix matcher and <h5>/<h6>
        // were missing; all three are block-level for line-break purposes.
        b"pre".as_slice(),
        b"h1".as_slice(),
        b"h2".as_slice(),
        b"h3".as_slice(),
        b"h4".as_slice(),
        b"h5".as_slice(),
        b"h6".as_slice(),
    ] {
        if name.eq_ignore_ascii_case(block_name) {
            return true;
        }
    }
    false
}
