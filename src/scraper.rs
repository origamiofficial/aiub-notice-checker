use std::pin::pin;
use std::time::Duration;

use chrono::NaiveDate;
use futures_util::future::{select, Either};
use tl::{Node, Parser, ParserOptions};
use worker::observability::{enter_span, SpanStatus};
use worker::{
    AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect,
    Response, Result, Url,
};

use crate::config::{
    BASE_HOST, BASE_URL, HTTP_TIMEOUT_MS, LIST_PAGE_SIZE, MAX_HTML_BYTES, MAX_REDIRECTS,
    MAX_URL_LENGTH, MIN_LISTING_DATE_COVERAGE, MIN_LISTING_DESC_COVERAGE, NOTICE_PATH,
};
use crate::models::Notice;

pub async fn send_with_timeout(fetch: Fetch, timeout_ms: u64) -> Result<Response> {
    let controller = AbortController::default();
    let signal = controller.signal();
    let req_fut = pin!(fetch.send_with_signal(&signal));
    let timer_fut = pin!(Delay::from(Duration::from_millis(timeout_ms)));

    match select(req_fut, timer_fut).await {
        Either::Left((res, _)) => res,
        Either::Right(((), _)) => {
            controller.abort();
            Err(worker::Error::RustError(format!(
                "HTTP request timed out after {timeout_ms}ms"
            )))
        }
    }
}

pub fn clean_text(raw: &str) -> String {
    let decoded = html_escape::decode_html_entities(raw);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn normalize_url(href: &str, base: &str, allow_external: bool) -> Option<String> {
    let trimmed = href.replace('\\', "/").trim().to_string();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let base_url = Url::parse(base).ok()?;
    let mut joined = base_url.join(&trimmed).ok()?;
    joined.set_fragment(None);

    let scheme = joined.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if joined.host_str().is_none() || !joined.username().is_empty() || joined.password().is_some() {
        return None;
    }

    let host_matches = joined
        .host_str()
        .map(|h| h.eq_ignore_ascii_case(BASE_HOST))
        .unwrap_or(false);

    if host_matches && scheme == "http" {
        joined.set_scheme("https").ok()?;
    } else if !host_matches && !allow_external {
        return None;
    }

    let out = joined.to_string();
    if out.len() > MAX_URL_LENGTH {
        return None;
    }
    Some(out)
}

pub fn parse_date_text(raw: &str) -> Option<String> {
    let cleaned = clean_text(raw).replace("Sept", "Sep");
    if cleaned.is_empty() {
        return None;
    }
    for fmt in ["%d %b %Y", "%d %B %Y", "%Y-%m-%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(&cleaned, fmt) {
            return Some(d.format("%Y-%m-%d").to_string());
        }
    }
    None
}

pub async fn fetch_html(url: &str) -> Result<String> {
    let mut target = normalize_url(url, BASE_URL, false)
        .ok_or_else(|| worker::Error::RustError(format!("Unsafe AIUB URL: {url}")))?;

    for _ in 0..MAX_REDIRECTS {
        let mut headers = Headers::new();
        headers.set(
            "User-Agent",
            "aiub-notice-checker/6.2 (+https://github.com/origamiofficial/aiub-notice-checker)",
        )?;
        headers.set("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8")?;

        let mut init = RequestInit::new();
        init.with_method(Method::Get);
        init.with_headers(headers);
        init.with_redirect(RequestRedirect::Manual);

        let req = Request::new_with_init(&target, &init)?;
        let mut resp = send_with_timeout(Fetch::Request(req), HTTP_TIMEOUT_MS).await?;
        let status = resp.status_code();

        if (300..400).contains(&status) {
            let location = resp.headers().get("Location")?.unwrap_or_default();
            target = normalize_url(&location, &target, false).ok_or_else(|| {
                worker::Error::RustError(format!("Unsafe redirect from {target} to {location}"))
            })?;
            continue;
        }

        if !(200..300).contains(&status) {
            return Err(worker::Error::RustError(format!(
                "AIUB HTTP {status} for {target}"
            )));
        }

        let content_type = resp
            .headers()
            .get("Content-Type")?
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        if content_type != "text/html" && content_type != "application/xhtml+xml" {
            return Err(worker::Error::RustError(format!(
                "Unexpected Content-Type '{content_type}' for {target}"
            )));
        }

        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_HTML_BYTES {
            return Err(worker::Error::RustError(format!(
                "HTML response exceeded {MAX_HTML_BYTES} bytes for {target}"
            )));
        }

        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }

    Err(worker::Error::RustError(format!(
        "Too many redirects while fetching {url}"
    )))
}

pub fn parse_listing_page(html: &str) -> Result<Vec<Notice>> {
    enter_span("parse_listing_html", |span| {
        span.set_attribute("html_bytes", html.len().to_string().as_str());
        let dom = tl::parse(html, ParserOptions::default()).map_err(|e| {
            let msg = format!("HTML parse error: {e}");
            span.set_status(SpanStatus::Error(Some(msg.clone())));
            worker::Error::RustError(msg)
        })?;
        let parser = dom.parser();

        let mut notices = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        for card_selector in ["div.notification", "article.notification"] {
            let Some(cards) = dom.query_selector(card_selector) else {
                continue;
            };
            for handle in cards {
                let Some(node) = handle.get(parser) else {
                    continue;
                };
                let Some(tag) = node.as_tag() else {
                    continue;
                };

                let title = first_selector_text(tag, parser, &["h2.title", "h3.title", "h2"]);
                let description = first_selector_text(tag, parser, &["p.desc", "div.desc", "p"]);
                let date_raw =
                    first_selector_text(tag, parser, &["div.date-custom", "span.date-custom", "time"]);
                let href =
                    first_selector_attr(tag, parser, &["a.info-link", "a"], "href").unwrap_or_default();

                let Some(url) = normalize_url(&href, BASE_URL, false) else {
                    continue;
                };
                if title.is_empty() || !seen_urls.insert(url.clone()) {
                    continue;
                }

                notices.push(Notice {
                    title,
                    description,
                    url,
                    published_date: parse_date_text(&date_raw),
                    body_text: String::new(),
                    attachments: Vec::new(),
                    detail_fetched: false,
                });
            }
            if !notices.is_empty() {
                break;
            }
        }

        if notices.is_empty() {
            let msg = "AIUB listing has no notice cards".to_string();
            span.set_status(SpanStatus::Error(Some(msg.clone())));
            return Err(worker::Error::RustError(msg));
        }

        if notices.len() >= 5 {
            let dated = notices.iter().filter(|n| n.published_date.is_some()).count();
            if (dated as f64) / (notices.len() as f64) < MIN_LISTING_DATE_COVERAGE {
                let msg = format!("AIUB listing date coverage dropped to {dated}/{}", notices.len());
                span.set_status(SpanStatus::Error(Some(msg.clone())));
                return Err(worker::Error::RustError(msg));
            }
            let described = notices.iter().filter(|n| !n.description.is_empty()).count();
            if (described as f64) / (notices.len() as f64) < MIN_LISTING_DESC_COVERAGE {
                let msg = format!(
                    "AIUB listing description coverage dropped to {described}/{}",
                    notices.len()
                );
                span.set_status(SpanStatus::Error(Some(msg.clone())));
                return Err(worker::Error::RustError(msg));
            }
        }

        span.set_attribute("notice_count", notices.len().to_string().as_str());
        Ok(notices)
    })
}

pub fn parse_detail_page(html: &str, fallback: &Notice) -> Result<Notice> {
    enter_span("parse_detail_html", |span| {
        span.set_attribute("url", fallback.url.as_str());
        let dom = tl::parse(html, ParserOptions::default()).map_err(|e| {
            let msg = format!("Detail parse error: {e}");
            span.set_status(SpanStatus::Error(Some(msg.clone())));
            worker::Error::RustError(msg)
        })?;
        let parser = dom.parser();

        let mut title = dom
            .get_element_by_id("dynamicHeading")
            .and_then(|h| h.get(parser))
            .map(|n| clean_text(&n.inner_text(parser)))
            .unwrap_or_default();

        if title.is_empty() {
            for sel in ["h1.header-title", "h1.title", "h1"] {
                if let Some(mut iter) = dom.query_selector(sel) {
                    if let Some(text) = iter
                        .next()
                        .and_then(|h| h.get(parser))
                        .map(|n| clean_text(&n.inner_text(parser)))
                        .filter(|t| !t.is_empty())
                    {
                        title = text;
                        break;
                    }
                }
            }
        }

        if title.is_empty() {
            let msg = format!("Missing heading on {}", fallback.url);
            span.set_status(SpanStatus::Error(Some(msg.clone())));
            return Err(worker::Error::RustError(msg));
        }

        let mut body_parts = Vec::new();
        let mut attachments = Vec::new();

        if let Some(pages) = dom.query_selector("div.notice-page") {
            for page_handle in pages {
                let Some(page_tag) = page_handle.get(parser).and_then(|n| n.as_tag()) else {
                    continue;
                };
                if let Some(cols) = page_tag.query_selector(parser, "div.question-column") {
                    for handle in cols {
                        let Some(node) = handle.get(parser) else {
                            continue;
                        };
                        let Some(tag) = node.as_tag() else {
                            continue;
                        };
                        let is_sticky = tag
                            .attributes()
                            .class()
                            .map(|c| {
                                c.as_utf8_str()
                                    .split_whitespace()
                                    .any(|w| w == "notice-sticky-header")
                            })
                            .unwrap_or(false);
                        if is_sticky {
                            continue;
                        }

                        extract_node_content(
                            node,
                            tag,
                            parser,
                            &fallback.url,
                            &mut body_parts,
                            &mut attachments,
                        );
                    }
                }
            }
        }

        if body_parts.is_empty() && attachments.is_empty() {
            for fallback_sel in [
                "div.notice-details",
                "div.notice-content",
                "div.notice-page",
                "article",
            ] {
                if let Some(containers) = dom.query_selector(fallback_sel) {
                    for handle in containers {
                        let Some(node) = handle.get(parser) else {
                            continue;
                        };
                        let Some(tag) = node.as_tag() else {
                            continue;
                        };
                        extract_node_content(
                            node,
                            tag,
                            parser,
                            &fallback.url,
                            &mut body_parts,
                            &mut attachments,
                        );
                    }
                }
                if !body_parts.is_empty() || !attachments.is_empty() {
                    break;
                }
            }
        }

        let body_text = body_parts.join("\n\n");
        if body_text.is_empty() && attachments.is_empty() {
            let msg = format!("AIUB detail page has no usable content: {}", fallback.url);
            span.set_status(SpanStatus::Error(Some(msg.clone())));
            return Err(worker::Error::RustError(msg));
        }

        Ok(Notice {
            title,
            description: fallback.description.clone(),
            url: fallback.url.clone(),
            published_date: fallback.published_date.clone(),
            body_text,
            attachments,
            detail_fetched: true,
        })
    })
}

pub fn listing_page_url(page_no: usize) -> String {
    format!("{BASE_URL}{NOTICE_PATH}?pageNo={page_no}&pageSize={LIST_PAGE_SIZE}")
}

fn extract_node_content(
    node: &Node<'_>,
    tag: &tl::HTMLTag<'_>,
    parser: &Parser<'_>,
    page_url: &str,
    body_parts: &mut Vec<String>,
    attachments: &mut Vec<String>,
) {
    let mut text_buf = String::new();
    collect_visible_text(node, parser, &mut text_buf);
    let cleaned = clean_text(&text_buf);
    if !cleaned.is_empty() {
        body_parts.push(cleaned);
    }
    collect_attachments(tag, parser, page_url, attachments);
}

fn first_selector_text(root: &tl::HTMLTag<'_>, parser: &Parser<'_>, selectors: &[&str]) -> String {
    for selector in selectors {
        let Some(mut iter) = root.query_selector(parser, selector) else {
            continue;
        };
        if let Some(text) = iter
            .next()
            .and_then(|h| h.get(parser))
            .map(|n| clean_text(&n.inner_text(parser)))
            .filter(|t| !t.is_empty())
        {
            return text;
        }
    }
    String::new()
}

fn first_selector_attr(
    root: &tl::HTMLTag<'_>,
    parser: &Parser<'_>,
    selectors: &[&str],
    attr: &str,
) -> Option<String> {
    for selector in selectors {
        let Some(mut iter) = root.query_selector(parser, selector) else {
            continue;
        };
        if let Some(val) = iter
            .next()
            .and_then(|h| h.get(parser))
            .and_then(|n| n.as_tag())
            .and_then(|t| t.attributes().get(attr).flatten())
            .map(|v| v.as_utf8_str().into_owned())
            .filter(|s| !s.is_empty())
        {
            return Some(val);
        }
    }
    None
}

fn collect_visible_text(node: &Node<'_>, parser: &Parser<'_>, out: &mut String) {
    match node {
        Node::Raw(bytes) => {
            out.push_str(&bytes.as_utf8_str());
            out.push(' ');
        }
        Node::Tag(tag) => {
            let name = tag.name().as_utf8_str();
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "script" | "style" | "noscript" | "template"
            ) {
                return;
            }
            let is_sticky = tag
                .attributes()
                .class()
                .map(|c| {
                    c.as_utf8_str()
                        .split_whitespace()
                        .any(|w| w == "notice-sticky-header")
                })
                .unwrap_or(false);
            if is_sticky {
                return;
            }
            for child_handle in tag.children().top().iter() {
                if let Some(child) = child_handle.get(parser) {
                    collect_visible_text(child, parser, out);
                }
            }
        }
        Node::Comment(_) => {}
    }
}

fn collect_attachments(
    root: &tl::HTMLTag<'_>,
    parser: &Parser<'_>,
    page_url: &str,
    out: &mut Vec<String>,
) {
    let mut push_href = |raw: &str| {
        if let Some(norm) = normalize_url(raw, page_url, true) {
            let norm_trimmed = norm.trim_end_matches('/');
            if norm_trimmed != BASE_URL
                && norm_trimmed != page_url.trim_end_matches('/')
                && !out.contains(&norm)
            {
                out.push(norm);
            }
        }
    };

    if let Some(links) = root.query_selector(parser, "a[href]") {
        for h in links {
            if let Some(tag) = h.get(parser).and_then(|n| n.as_tag()) {
                if let Some(Some(href)) = tag.attributes().get("href") {
                    push_href(&href.as_utf8_str());
                }
            }
        }
    }
    if let Some(imgs) = root.query_selector(parser, "img") {
        for h in imgs {
            if let Some(tag) = h.get(parser).and_then(|n| n.as_tag()) {
                for attr in ["src", "data-src"] {
                    if let Some(Some(src)) = tag.attributes().get(attr) {
                        push_href(&src.as_utf8_str());
                    }
                }
            }
        }
    }
}