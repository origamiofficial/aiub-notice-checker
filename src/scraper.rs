use chrono::NaiveDate;
use tl::{Node, Parser, ParserOptions, VDom};
use url::Url;
use worker::{Fetch, Headers, Method, Request, RequestInit, Result};

use crate::config::{
    BASE_HOST, BASE_URL, LIST_PAGE_SIZE, MAX_HTML_BYTES, MAX_URL_LENGTH,
    MIN_LISTING_DATE_COVERAGE, MIN_LISTING_DESC_COVERAGE, NOTICE_PATH,
};
use crate::models::Notice;

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
    for fmt in ["%d %b %Y", "%d %B %Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(&cleaned, fmt) {
            return Some(d.format("%Y-%m-%d").to_string());
        }
    }
    None
}

pub async fn fetch_html(url: &str) -> Result<String> {
    let target = normalize_url(url, BASE_URL, false)
        .ok_or_else(|| worker::Error::RustError(format!("Unsafe AIUB URL: {url}")))?;

    let mut headers = Headers::new();
    headers.set("User-Agent", "aiub-notice-checker/6.0 (+https://github.com/origamiofficial/aiub-notice-checker)")?;
    headers.set("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8")?;

    let mut init = RequestInit::new();
    init.with_method(Method::Get);
    init.with_headers(headers);

    let req = Request::new_with_init(&target, &init)?;
    let mut resp = Fetch::Request(req).send().await?;

    if resp.status_code() < 200 || resp.status_code() >= 300 {
        return Err(worker::Error::RustError(format!(
            "AIUB HTTP {} for {target}",
            resp.status_code()
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

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn parse_listing_page(html: &str) -> Result<Vec<Notice>> {
    let dom = tl::parse(html, ParserOptions::default())
        .map_err(|e| worker::Error::RustError(format!("HTML parse error: {e}")))?;
    let parser = dom.parser();

    let cards = dom
        .query_selector("div.notification")
        .ok_or_else(|| worker::Error::RustError("Invalid selector".into()))?;

    let mut notices = Vec::new();
    for handle in cards {
        let Some(node) = handle.get(parser) else {
            continue;
        };
        let Some(tag) = node.as_tag() else {
            continue;
        };

        let title = first_selector_text(tag, parser, "h2.title");
        let description = first_selector_text(tag, parser, "p.desc");
        let date_raw = first_selector_text(tag, parser, "div.date-custom");
        let href = first_selector_attr(tag, parser, "a.info-link", "href").unwrap_or_default();

        let Some(url) = normalize_url(&href, BASE_URL, false) else {
            continue;
        };
        if title.is_empty() {
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

    if notices.is_empty() {
        return Err(worker::Error::RustError("AIUB listing has no notice cards".into()));
    }

    if notices.len() >= 5 {
        let dated = notices.iter().filter(|n| n.published_date.is_some()).count();
        if (dated as f64) / (notices.len() as f64) < MIN_LISTING_DATE_COVERAGE {
            return Err(worker::Error::RustError(format!(
                "AIUB listing date coverage dropped to {dated}/{}",
                notices.len()
            )));
        }
        let described = notices.iter().filter(|n| !n.description.is_empty()).count();
        if (described as f64) / (notices.len() as f64) < MIN_LISTING_DESC_COVERAGE {
            return Err(worker::Error::RustError(format!(
                "AIUB listing description coverage dropped to {described}/{}",
                notices.len()
            )));
        }
    }

    Ok(notices)
}

pub fn parse_detail_page(html: &str, fallback: &Notice) -> Result<Notice> {
    let dom = tl::parse(html, ParserOptions::default())
        .map_err(|e| worker::Error::RustError(format!("Detail parse error: {e}")))?;
    let parser = dom.parser();

    let title = dom
        .get_element_by_id("dynamicHeading")
        .and_then(|h| h.get(parser))
        .map(|n| clean_text(&n.inner_text(parser)))
        .unwrap_or_default();

    if title.is_empty() {
        return Err(worker::Error::RustError(format!(
            "Missing #dynamicHeading on {}",
            fallback.url
        )));
    }

    let mut body_parts = Vec::new();
    let mut attachments = Vec::new();

    if let Some(cols) = dom.query_selector("div.notice-page div.question-column") {
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
                .map(|c| c.as_utf8_str().split_whitespace().any(|w| w == "notice-sticky-header"))
                .unwrap_or(false);
            if is_sticky {
                continue;
            }

            let mut text_buf = String::new();
            collect_visible_text(node, parser, &mut text_buf);
            let cleaned = clean_text(&text_buf);
            if !cleaned.is_empty() {
                body_parts.push(cleaned);
            }

            collect_attachments(tag, parser, &fallback.url, &mut attachments);
        }
    }

    let body_text = body_parts.join("\n\n");
    if body_text.is_empty() && attachments.is_empty() {
        return Err(worker::Error::RustError(format!(
            "AIUB detail page has no usable content: {}",
            fallback.url
        )));
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
}

pub fn listing_page_url(page_no: usize) -> String {
    format!("{BASE_URL}{NOTICE_PATH}?pageNo={page_no}&pageSize={LIST_PAGE_SIZE}")
}

fn first_selector_text(root: &tl::HTMLTag<'_>, parser: &Parser<'_>, selector: &str) -> String {
    let Some(mut iter) = root.query_selector(parser, selector) else {
        return String::new();
    };
    iter.next()
        .and_then(|h| h.get(parser))
        .map(|n| clean_text(&n.inner_text(parser)))
        .unwrap_or_default()
}

fn first_selector_attr(
    root: &tl::HTMLTag<'_>,
    parser: &Parser<'_>,
    selector: &str,
    attr: &str,
) -> Option<String> {
    let mut iter = root.query_selector(parser, selector)?;
    let handle = iter.next()?;
    let tag = handle.get(parser)?.as_tag()?;
    tag.attributes()
        .get(attr)
        .flatten()
        .map(|v| v.as_utf8_str().into_owned())
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
            if norm_trimmed != BASE_URL && norm_trimmed != page_url.trim_end_matches('/') {
                if !out.contains(&norm) {
                    out.push(norm);
                }
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