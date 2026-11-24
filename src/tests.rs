// Unit tests for the scraper and version parsing logic.
//
// These run as `#[cfg(test)]` within the crate so they can test `pub(crate)` helpers.
// The `worker` crate's types (Fetch, etc.) are not available in a standard test harness,
// so we test the pure parsing functions that don't require the Workers runtime.

#[cfg(test)]
mod scraper_tests {
    use crate::scraper::{
        USER_AGENT, is_block_tag, normalize_aiub_url, normalize_notice_url, parse_detail_html,
        parse_detail_with_reason, parse_listing_html,
    };

    // L1: <pre>, <h5> and <h6> are block tags; exact-name matching must not
    // false-positive on longer names such as <prefix> or <pfoo>.
    #[test]
    fn is_block_tag_includes_pre_h5_h6() {
        assert!(is_block_tag(b"<pre>"));
        assert!(is_block_tag(b"</pre>"));
        assert!(is_block_tag(b"<h5>"));
        assert!(is_block_tag(b"<h6 class=\"x\">"));
        assert!(is_block_tag(b"<h5/>"));
        assert!(is_block_tag(b"<p>"));
        assert!(!is_block_tag(b"<prefix>"));
        assert!(!is_block_tag(b"<pfoo>"));
        assert!(!is_block_tag(b"<span>"));
        assert!(!is_block_tag(b"not a tag"));
    }

    #[test]
    fn user_agent_matches_cargo_package_version() {
        assert_eq!(
            USER_AGENT,
            format!(
                "aiub-notice-checker/{} (+https://github.com/origamiofficial/aiub-notice-checker)",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    // ── Listing page parsing ─────────────────────────────────────────

    #[test]
    fn parse_listing_empty_html() {
        let notices = parse_listing_html("");
        assert!(notices.is_empty(), "empty HTML should yield no notices");
    }

    #[test]
    fn parse_listing_no_notification_divs() {
        let html = "<html><body><p>No notices here</p></body></html>";
        let notices = parse_listing_html(html);
        assert!(notices.is_empty());
    }

    #[test]
    fn parse_listing_single_notification() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="/notice/example-notice-123">Link</a>
            <h2 class="title">Test Notice Title</h2>
            <p class="desc">Short description of the notice.</p>
            <div class="date-custom">15 November 2026</div>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1, "should parse exactly one notice");
        assert_eq!(notices[0].title, "Test Notice Title");
        assert_eq!(notices[0].description, "Short description of the notice.");
        assert!(
            notices[0].url.contains("notice/example-notice-123"),
            "URL should contain the href path"
        );
        assert_eq!(
            notices[0].published_date,
            Some("2026-11-15".to_string()),
            "date should be parsed to ISO format"
        );
    }

    // ── Audit #6: an unparsable <time> must not block the .date fallback ──

    #[test]
    fn parse_listing_time_holding_clock_time_falls_back_to_date() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="/notice/clock-time-date">Link</a>
            <h2 class="title">Clock Time Notice</h2>
            <p class="desc">Short description.</p>
            <time>10:30 AM</time>
            <div class="date">24 November 2026</div>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].published_date,
            Some("2026-11-24".to_string()),
            "a <time> holding a clock time must fall through to .date"
        );
    }

    #[test]
    fn parse_listing_multiple_notifications() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="/notice/first-notice">Link</a>
            <h2 class="title">First Notice</h2>
            <p class="desc">Description 1</p>
        </div>
        <div class="notification">
            <a class="info-link" href="/notice/second-notice">Link</a>
            <h2 class="title">Second Notice</h2>
            <p class="desc">Description 2</p>
        </div>
        <div class="notification">
            <a class="info-link" href="/notice/third-notice">Link</a>
            <h2 class="title">Third Notice</h2>
            <p class="desc">Description 3</p>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0].title, "First Notice");
        assert_eq!(notices[1].title, "Second Notice");
        assert_eq!(notices[2].title, "Third Notice");
    }

    #[test]
    fn parse_listing_skips_entries_without_title() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="/notice/no-title">Link</a>
            <p class="desc">Has description but no title</p>
        </div>
        <div class="notification">
            <a class="info-link" href="/notice/with-title">Link</a>
            <h2 class="title">Valid Title</h2>
            <p class="desc">Valid description</p>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Valid Title");
    }

    #[test]
    fn parse_listing_skips_invalid_urls() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="javascript:void(0)">Link</a>
            <h2 class="title">JS Link</h2>
        </div>
        <div class="notification">
            <a class="info-link" href="mailto:admin@aiub.edu">Link</a>
            <h2 class="title">Mail Link</h2>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert!(
            notices.is_empty(),
            "javascript: and mailto: links should be skipped"
        );
    }

    #[test]
    fn parse_listing_fallback_to_card_class() {
        // M16: a `.card` only counts as a notice card when it carries an
        // `a.info-link` / `a.notification-link`.
        let html = r#"
        <html><body>
        <div class="card">
            <a class="info-link" href="/notice/card-notice">Link</a>
            <h2 class="title">Card Notice</h2>
            <p class="desc">Card description</p>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Card Notice");
    }

    #[test]
    fn parse_listing_card_without_info_link_is_ignored() {
        // M16: a `.card` with only a generic link (promo tile, event
        // card, sidebar widget) must not be misparsed as a notice.
        let html = r#"
        <html><body>
        <div class="card">
            <a href="/notice/card-notice">Link</a>
            <h2 class="title">Card Notice</h2>
            <p class="desc">Card description</p>
        </div>
        <div class="card">
            <a href="https://example.com/promo">Promo</a>
            <h2 class="title">Promo Card</h2>
            <p class="desc">Not a notice</p>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert!(
            notices.is_empty(),
            "cards without an info-link must not be parsed as notices"
        );
    }

    #[test]
    fn parse_listing_fallback_li_in_event_list() {
        let html = r#"
        <html><body>
        <ul class="event-list">
            <li>
                <a class="info-link" href="/notice/event-notice">Link</a>
                <h2 class="title">Event List Notice</h2>
                <p class="desc">From ul.event-list fallback</p>
            </li>
        </ul>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Event List Notice");
    }

    #[test]
    fn parse_listing_whitespace_normalized() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="/notice/whitespace-test">Link</a>
            <h2 class="title">  Title   with    extra   spaces  </h2>
            <p class="desc">  Desc  with   extra   spaces  </p>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Title with extra spaces");
        assert_eq!(notices[0].description, "Desc with extra spaces");
    }

    #[test]
    fn clean_whitespace_strips_word_and_cms_invisible_chars() {
        use crate::scraper::clean_whitespace;
        // Soft hyphen (U+00AD)
        assert_eq!(
            clean_whitespace("Bangladesh\u{00AD} SET B EXAM"),
            "Bangladesh SET B EXAM"
        );
        assert_eq!(clean_whitespace("Bangladesh\u{00AD}SET"), "BangladeshSET");
        // Left-to-right mark (U+200E) and Right-to-left mark (U+200F)
        assert_eq!(
            clean_whitespace("\u{200E}Admission & Registration Procedure"),
            "Admission & Registration Procedure"
        );
        assert_eq!(
            clean_whitespace("Procedure\u{200F} Schedule"),
            "Procedure Schedule"
        );
        // Zero-width space, non-joiner, joiner (U+200B..=U+200D)
        assert_eq!(
            clean_whitespace("Word\u{200B}with\u{200C}zero\u{200D}width"),
            "Wordwithzerowidth"
        );
        // Byte order mark (U+FEFF)
        assert_eq!(
            clean_whitespace("\u{FEFF}Leading BOM and \u{FEFF}middle BOM"),
            "Leading BOM and middle BOM"
        );
        // Only invisible chars
        assert_eq!(
            clean_whitespace("\u{00AD}\u{200B}\u{200C}\u{200D}\u{200E}\u{200F}\u{FEFF}"),
            ""
        );
    }

    // ── Date parsing ─────────────────────────────────────────────────

    #[test]
    fn parse_listing_date_formats() {
        // "DD Month YYYY" format
        let html = |date: &str| {
            format!(
                r#"<html><body>
                <div class="notification">
                    <a class="info-link" href="/notice/date-test">Link</a>
                    <h2 class="title">Date Test</h2>
                    <div class="date-custom">{date}</div>
                </div>
                </body></html>"#
            )
        };

        let notices = parse_listing_html(&html("15 November 2026"));
        assert_eq!(notices[0].published_date, Some("2026-11-15".to_string()));

        let notices = parse_listing_html(&html("1 Jan 2025"));
        assert_eq!(notices[0].published_date, Some("2025-01-01".to_string()));

        let notices = parse_listing_html(&html("25 December 2026"));
        assert_eq!(notices[0].published_date, Some("2026-12-25".to_string()));

        let notices = parse_listing_html(&html("November 15, 2026"));
        assert_eq!(notices[0].published_date, Some("2026-11-15".to_string()));

        let notices = parse_listing_html(&html("15th November 2026"));
        assert_eq!(notices[0].published_date, Some("2026-11-15".to_string()));

        let notices = parse_listing_html(&html("15-Nov-2026"));
        assert_eq!(notices[0].published_date, Some("2026-11-15".to_string()));
    }

    #[test]
    fn parse_listing_date_from_separate_fields() {
        let html = r#"
        <html><body>
        <div class="notification">
            <a class="info-link" href="/notice/split-date">Link</a>
            <h2 class="title">Split Date</h2>
            <span class="day">15</span>
            <span class="month">Nov</span>
            <span class="year">2026</span>
        </div>
        </body></html>
        "#;
        let notices = parse_listing_html(html);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].published_date, Some("2026-11-15".to_string()));
    }

    // ── Detail page parsing ──────────────────────────────────────────

    #[test]
    fn parse_detail_empty_html() {
        let detail = parse_detail_html("", "https://www.aiub.edu/notice/test");
        assert!(detail.is_none(), "empty HTML should return None");
    }

    #[test]
    fn parse_detail_with_title_and_body() {
        let html = r#"
        <html><body>
        <h1 id="dynamicHeading">Full Detail Title</h1>
        <div class="notice-page">
            <div class="question-column notice-sticky-header">
                <div class="date-custom">15 November 2026</div>
            </div>
            <div class="question-column">
                <p>This is the body text of the notice.</p>
            </div>
        </div>
        </body></html>
        "#;
        let detail =
            parse_detail_html(html, "https://www.aiub.edu/notice/test").expect("should parse");
        assert_eq!(detail.title, Some("Full Detail Title".to_string()));
        assert!(
            detail.body_text.contains("body text of the notice"),
            "body_text should contain the paragraph text"
        );
        assert_eq!(
            detail.published_date,
            Some("2026-11-15".to_string()),
            "date should be extracted from sticky header"
        );
    }

    // ── Audit #6: an unparsable <time> must not block the .date fallback ──

    #[test]
    fn parse_detail_time_holding_clock_time_falls_back_to_date() {
        let html = r#"
        <html><body>
        <h1 id="dynamicHeading">Clock Time Detail</h1>
        <div class="notice-page">
            <div class="question-column notice-sticky-header">
                <time>10:30 AM</time>
                <div class="date">24 November 2026</div>
            </div>
            <div class="question-column">
                <p>This is the body text of the notice.</p>
            </div>
        </div>
        </body></html>
        "#;
        let detail =
            parse_detail_html(html, "https://www.aiub.edu/notice/test").expect("should parse");
        assert_eq!(
            detail.published_date,
            Some("2026-11-24".to_string()),
            "a <time> holding a clock time must fall through to .date"
        );
    }

    #[test]
    fn parse_detail_extracts_attachments() {
        let html = r#"
        <html><body>
        <div class="notice-page">
            <div class="question-column">
                <p>Notice with attachments</p>
                <a href="/uploads/document.pdf">Download PDF</a>
                <img src="/images/notice-image.jpg" />
            </div>
        </div>
        </body></html>
        "#;
        let detail =
            parse_detail_html(html, "https://www.aiub.edu/notice/test").expect("should parse");
        assert!(
            detail.attachments.len() >= 2,
            "should extract both a and img attachments, got: {:?}",
            detail.attachments
        );
    }

    #[test]
    fn parse_detail_ignores_script_style_content() {
        let html = r#"
        <html><body>
        <div class="notice-page">
            <div class="question-column">
                <p>Visible text</p>
                <script>var x = "invisible script content";</script>
                <style>.hidden { display: none; }</style>
                <p>More visible text</p>
            </div>
        </div>
        </body></html>
        "#;
        let detail =
            parse_detail_html(html, "https://www.aiub.edu/notice/test").expect("should parse");
        assert!(
            !detail.body_text.contains("invisible script content"),
            "script content should be stripped"
        );
        assert!(
            !detail.body_text.contains("display: none"),
            "style content should be stripped"
        );
        assert!(
            detail.body_text.contains("Visible text"),
            "visible text should remain"
        );
    }

    #[test]
    fn parse_detail_style_before_script_does_not_swallow_middle_text() {
        let html = r#"
        <html><body>
        <div class="notice-page">
            <div class="question-column">
                <style>.hidden { display: none; }</style>
                <p>Crucial middle text that must survive</p>
                <script>var x = "invisible script content";</script>
            </div>
        </div>
        </body></html>
        "#;
        let detail =
            parse_detail_html(html, "https://www.aiub.edu/notice/test").expect("should parse");
        assert!(
            detail
                .body_text
                .contains("Crucial middle text that must survive"),
            "middle text was swallowed between style and script tags! Got: {:?}",
            detail.body_text
        );
    }

    #[test]
    fn parse_detail_returns_none_without_question_column() {
        let html = r#"
        <html><body>
        <h1 id="dynamicHeading">Title Only</h1>
        <div class="notice-page">
            <p>No question-column div here</p>
        </div>
        </body></html>
        "#;
        let detail = parse_detail_html(html, "https://www.aiub.edu/notice/test");
        assert!(
            detail.is_none(),
            "should return None when no question-column exists"
        );
    }

    #[test]
    fn parse_detail_separates_sticky_header_from_body() {
        let html = r#"
        <html><body>
        <div class="notice-page">
            <div class="question-column notice-sticky-header">
                <p>Sticky header text</p>
            </div>
            <div class="question-column">
                <p>Actual body content</p>
            </div>
        </div>
        </body></html>
        "#;
        let detail =
            parse_detail_html(html, "https://www.aiub.edu/notice/test").expect("should parse");
        assert!(
            detail.body_text.contains("Actual body content"),
            "body should contain non-sticky content"
        );
        // The sticky header body goes into legacy_sticky_body
        assert!(
            detail.legacy_sticky_body.contains("Sticky header text"),
            "legacy_sticky_body should contain sticky header text"
        );
    }

    #[test]
    fn parse_detail_with_reason_reports_empty_html() {
        let err = parse_detail_with_reason("", "https://www.aiub.edu/test")
            .expect_err("should return Err on empty html");
        assert_eq!(err, "HTML content is empty");
    }

    #[test]
    fn parse_detail_with_reason_reports_missing_question_column() {
        let html =
            "<html><body><div class='notice-page'><p>No question column</p></div></body></html>";
        let err = parse_detail_with_reason(html, "https://www.aiub.edu/test")
            .expect_err("should return Err when question-column is missing");
        assert_eq!(err, "missing non-sticky question-column container");
    }

    #[test]
    fn parse_detail_with_reason_reports_empty_content_in_question_column() {
        let html = "<html><body><div class='notice-page'><div class='question-column'>   </div></div></body></html>";
        let err = parse_detail_with_reason(html, "https://www.aiub.edu/test")
            .expect_err("should return Err when question-column has no text or attachments");
        assert_eq!(err, "question-column contained no text or attachments");
    }

    #[test]
    fn parse_detail_with_reason_succeeds_on_valid_detail() {
        let html = r#"
        <html><body>
        <h1 id="dynamicHeading">Notice Heading</h1>
        <div class="notice-page">
            <div class="question-column">
                <p>Valid text</p>
            </div>
        </div>
        </body></html>
        "#;
        let detail = parse_detail_with_reason(html, "https://www.aiub.edu/test")
            .expect("should succeed on valid detail");
        assert_eq!(detail.title, Some("Notice Heading".to_string()));
        assert_eq!(detail.body_text, "Valid text");
    }

    #[test]
    fn parse_detail_strips_invisible_unicode_entities() {
        let html = r#"
        <html><body>
        <h1 id="dynamicHeading">&lrm;Admission &amp; Registration Procedure</h1>
        <div class="notice-page">
            <div class="question-column">
                <p>American International University &ndash; Bangladesh&shy; SET B EXAM</p>
            </div>
        </div>
        </body></html>
        "#;
        let detail = parse_detail_with_reason(html, "https://www.aiub.edu/test")
            .expect("should succeed on valid detail");
        assert_eq!(
            detail.title,
            Some("Admission & Registration Procedure".to_string())
        );
        assert!(!detail.title.as_ref().unwrap().contains('\u{200E}'));
        assert_eq!(
            detail.body_text,
            "American International University – Bangladesh SET B EXAM"
        );
        assert!(!detail.body_text.contains('\u{00AD}'));
    }

    // ── URL normalization ────────────────────────────────────────────

    #[test]
    fn normalize_notice_url_relative_path() {
        let url = normalize_notice_url("/notice/some-notice-123");
        assert_eq!(url, "https://www.aiub.edu/notice/some-notice-123");
    }

    #[test]
    fn normalize_notice_url_absolute_aiub() {
        let url = normalize_notice_url("https://www.aiub.edu/notice/another-notice");
        assert_eq!(url, "https://www.aiub.edu/notice/another-notice");
    }

    #[test]
    fn normalize_notice_url_strips_fragment() {
        let url = normalize_notice_url("https://www.aiub.edu/notice/test#section1");
        assert_eq!(url, "https://www.aiub.edu/notice/test");
    }

    #[test]
    fn normalize_notice_url_upgrades_http_to_https() {
        let url = normalize_notice_url("http://www.aiub.edu/notice/test");
        assert_eq!(url, "https://www.aiub.edu/notice/test");
    }

    #[test]
    fn normalize_notice_url_rejects_external() {
        let url = normalize_notice_url("https://evil.com/notice/test");
        assert!(url.is_empty(), "external URLs should be rejected");
    }

    #[test]
    fn normalize_notice_url_rejects_javascript() {
        let url = normalize_notice_url("javascript:alert(1)");
        assert!(url.is_empty());
    }

    #[test]
    fn normalize_notice_url_rejects_mailto() {
        let url = normalize_notice_url("mailto:admin@aiub.edu");
        assert!(url.is_empty());
    }

    #[test]
    fn normalize_notice_url_empty_input() {
        let url = normalize_notice_url("");
        assert!(url.is_empty());
    }

    #[test]
    fn normalize_notice_url_hash_only() {
        let url = normalize_notice_url("#top");
        assert!(url.is_empty());
    }

    #[test]
    fn normalize_notice_url_strips_trailing_slash() {
        let url = normalize_notice_url("https://www.aiub.edu/notice/test/");
        assert_eq!(url, "https://www.aiub.edu/notice/test");
    }

    // ── Audit #5: scheme-less host hrefs and slash-before-query ──────

    #[test]
    fn normalize_notice_url_scheme_less_bare_host() {
        // "www.aiub.edu/notice/x" must not become
        // https://www.aiub.edu/www.aiub.edu/notice/x.
        let url = normalize_notice_url("www.aiub.edu/notice/bare-domain");
        assert_eq!(url, "https://www.aiub.edu/notice/bare-domain");
    }

    #[test]
    fn normalize_notice_url_slash_before_query_matches_plain_query() {
        let with_slash = normalize_notice_url("https://www.aiub.edu/notice/test/?a=1");
        let plain = normalize_notice_url("https://www.aiub.edu/notice/test?a=1");
        assert_eq!(with_slash, "https://www.aiub.edu/notice/test?a=1");
        assert_eq!(
            with_slash, plain,
            "one notice must not split into two links"
        );
    }

    #[test]
    fn normalize_notice_url_rejects_userinfo() {
        let url = normalize_notice_url("https://user:pass@www.aiub.edu/notice/test");
        assert!(url.is_empty(), "URLs with userinfo should be rejected");
    }

    #[test]
    fn normalize_aiub_url_allow_external() {
        let url = normalize_aiub_url(
            "https://external.example.com/file.pdf",
            "https://www.aiub.edu/notice/test",
            true,
        );
        assert_eq!(
            url, "https://external.example.com/file.pdf",
            "allow_external=true should permit non-AIUB URLs"
        );
    }

    #[test]
    fn normalize_aiub_url_disallow_external() {
        let url = normalize_aiub_url(
            "https://external.example.com/file.pdf",
            "https://www.aiub.edu/notice/test",
            false,
        );
        assert!(
            url.is_empty(),
            "allow_external=false should reject non-AIUB URLs"
        );
    }

    #[test]
    fn normalize_aiub_url_backslash_to_forward_slash() {
        let url = normalize_aiub_url("\\notice\\backslash-test", "https://www.aiub.edu/", false);
        assert!(url.contains("notice/backslash-test"));
    }
}

#[cfg(test)]
mod model_tests {
    use crate::models::{
        Notice, display_date, format_facebook_message, format_notice_message, titles_equivalent,
        truncate_text,
    };

    // ── Content hashing ──────────────────────────────────────────────

    #[test]
    fn content_hash_deterministic() {
        let notice = Notice {
            title: "Test Title".to_string(),
            description: "Test Description".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: Some("2026-11-15".to_string()),
            body_text: "Body text content".to_string(),
            attachments: vec!["https://www.aiub.edu/file.pdf".to_string()],
            detail_fetched: true,
        };
        let hash1 = notice.content_hash();
        let hash2 = notice.content_hash();
        assert_eq!(hash1, hash2, "content_hash must be deterministic");
        assert_eq!(hash1.len(), 64, "SHA-256 hex should be 64 chars");
    }

    #[test]
    fn content_hash_changes_with_title() {
        let make = |title: &str| Notice {
            title: title.to_string(),
            description: "Same".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "Same".to_string(),
            attachments: vec![],
            detail_fetched: true,
        };
        assert_ne!(
            make("Title A").content_hash(),
            make("Title B").content_hash()
        );
    }

    #[test]
    fn content_hash_changes_with_body() {
        let make = |body: &str| Notice {
            title: "Same".to_string(),
            description: "Same".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: body.to_string(),
            attachments: vec![],
            detail_fetched: true,
        };
        assert_ne!(make("Body A").content_hash(), make("Body B").content_hash());
    }

    #[test]
    fn content_hash_attachments_order_independent() {
        let make = |attachments: Vec<String>| Notice {
            title: "Same".to_string(),
            description: "Same".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "Same".to_string(),
            attachments,
            detail_fetched: true,
        };
        let hash_ab = make(vec!["a.pdf".to_string(), "b.pdf".to_string()]).content_hash();
        let hash_ba = make(vec!["b.pdf".to_string(), "a.pdf".to_string()]).content_hash();
        assert_eq!(hash_ab, hash_ba, "attachment order should not affect hash");
    }

    #[test]
    fn attachments_json_sorted_and_deduped() {
        let notice = Notice {
            title: "T".to_string(),
            description: "D".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "B".to_string(),
            attachments: vec![
                "z.pdf".to_string(),
                "a.pdf".to_string(),
                "z.pdf".to_string(),
            ],
            detail_fetched: true,
        };
        let json = notice.attachments_json();
        let parsed: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, vec!["a.pdf", "z.pdf"]);
    }

    // ── Title equivalence ────────────────────────────────────────────

    #[test]
    fn titles_equivalent_exact_match() {
        assert!(titles_equivalent("Hello World", "Hello World"));
    }

    #[test]
    fn titles_equivalent_case_insensitive() {
        assert!(titles_equivalent("hello world", "HELLO WORLD"));
    }

    #[test]
    fn titles_equivalent_with_ellipsis() {
        // Listing title is truncated with "..."
        assert!(titles_equivalent(
            "Important Notice About Exam...",
            "Important Notice About Examination Schedule"
        ));
    }

    #[test]
    fn titles_equivalent_ellipsis_prefix_mismatch() {
        // M1: "..." no longer means "equivalent to anything" — the
        // cached title must start with the visible prefix. A real edit
        // to a long title must not be masked as unchanged.
        assert!(!titles_equivalent(
            "Important Notice About Exam...",
            "Important Notice About Sports Day Events"
        ));
    }

    #[test]
    fn titles_equivalent_ellipsis_not_at_end() {
        // "..." in the middle is not a truncation marker.
        assert!(!titles_equivalent("Title... A", "Title B"));
    }

    #[test]
    fn titles_equivalent_ellipsis_empty_prefix() {
        assert!(!titles_equivalent("...", "Something Else"));
    }

    #[test]
    fn titles_not_equivalent_different_text() {
        assert!(!titles_equivalent("Title A", "Title B"));
    }

    // ── Text truncation ──────────────────────────────────────────────

    #[test]
    fn truncate_text_no_truncation_needed() {
        assert_eq!(truncate_text("Short text", 100), "Short text");
    }

    #[test]
    fn truncate_text_adds_ellipsis() {
        let result = truncate_text("This is a long text that needs truncation", 20);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 20);
    }

    #[test]
    fn truncate_text_trims_whitespace() {
        assert_eq!(truncate_text("  hello  ", 100), "hello");
    }

    #[test]
    fn truncate_text_handles_small_limits_without_exceeding() {
        assert_eq!(truncate_text("Hello", 0), "");
        assert_eq!(truncate_text("Hello", 1), "H");
        assert_eq!(truncate_text("Hello", 2), "He");
        assert_eq!(truncate_text("Hello", 3), "Hel");
        assert_eq!(truncate_text("Hello", 4), "H...");
        assert!(truncate_text("Hello", 2).chars().count() <= 2);
    }

    #[test]
    fn truncate_text_handles_unicode() {
        // Unicode chars should be counted by chars, not bytes
        let text = "বাংলা নোটিশ";
        let truncated = truncate_text(text, 5);
        assert!(truncated.chars().count() <= 5);
    }

    // ── Date display ─────────────────────────────────────────────────

    #[test]
    fn display_date_iso_to_human() {
        assert_eq!(display_date(Some("2026-11-15")), "15 Nov 2026");
    }

    #[test]
    fn display_date_none() {
        assert_eq!(display_date(None), "Unknown");
    }

    #[test]
    fn display_date_invalid_format() {
        assert_eq!(display_date(Some("not-a-date")), "not-a-date");
    }

    // ── Notice message formatting ────────────────────────────────────

    #[test]
    fn format_notice_message_basic() {
        let notice = Notice {
            title: "Test Notice".to_string(),
            description: "A test notice description".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: Some("2026-11-15".to_string()),
            body_text: "Full body text here".to_string(),
            attachments: vec![],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(msg.contains("Test Notice"));
        assert!(msg.contains("15 Nov 2026"));
        assert!(msg.contains("Full body text here"));
        assert!(msg.contains("https://www.aiub.edu/notice/test"));
        assert!(!msg.contains("[EDITED]"));
    }

    #[test]
    fn format_notice_message_edited() {
        let notice = Notice {
            title: "Edited Notice".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "Updated content".to_string(),
            attachments: vec![],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, true);
        assert!(msg.starts_with("[EDITED]"));
    }

    #[test]
    fn format_notice_message_uses_description_when_no_body() {
        let notice = Notice {
            title: "No Body".to_string(),
            description: "Description as fallback".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "".to_string(),
            attachments: vec![],
            detail_fetched: false,
        };
        let msg = format_notice_message(&notice, false);
        assert!(msg.contains("Description as fallback"));
    }

    #[test]
    fn format_notice_message_uses_placeholder_when_no_body_or_desc() {
        let notice = Notice {
            title: "Empty".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "".to_string(),
            attachments: vec![],
            detail_fetched: false,
        };
        let msg = format_notice_message(&notice, false);
        assert!(msg.contains("Please click the link for details."));
    }

    #[test]
    fn format_notice_message_surfaces_attachment_when_no_body() {
        let notice = Notice {
            title: "MBA Course Waivers".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/mba-waivers".to_string(),
            published_date: Some("2026-11-18".to_string()),
            body_text: "".to_string(),
            attachments: vec![
                "https://www.aiub.edu/Files/Uploads/webp/notice-mba-waivers.webp".to_string(),
            ],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(
            msg.contains("notice-mba-waivers.webp"),
            "should surface attachment URL when body and description are empty, got: {}",
            msg
        );
        assert!(
            !msg.contains("Please click the link for details."),
            "should NOT show generic placeholder when attachment is available"
        );
    }

    #[test]
    fn format_notice_message_treats_placeholder_desc_as_empty_and_surfaces_attachment() {
        let notice = Notice {
            title: "Poster Competition".to_string(),
            description: "No description available".to_string(),
            url: "https://www.aiub.edu/notice/poster".to_string(),
            published_date: Some("2026-08-13".to_string()),
            body_text: "".to_string(),
            attachments: vec![
                "https://www.aiub.edu/Files/Uploads/webp/poster-competition.webp".to_string(),
            ],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(
            msg.contains("poster-competition.webp"),
            "should surface attachment URL when description is 'No description available', got: {}",
            msg
        );
        assert!(
            !msg.contains("No description available"),
            "should NOT show literal 'No description available' fallback in Telegram"
        );
    }

    #[test]
    fn format_notice_message_treats_placeholder_desc_with_period_as_empty() {
        let notice = Notice {
            title: "Poster Competition Without Attachment".to_string(),
            description: "No description available.".to_string(),
            url: "https://www.aiub.edu/notice/poster".to_string(),
            published_date: Some("2026-08-13".to_string()),
            body_text: "".to_string(),
            attachments: vec![],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(
            msg.contains("Please click the link for details."),
            "should fall back to generic click link text when no attachment and placeholder desc"
        );
        assert!(
            !msg.contains("No description available"),
            "should NOT show literal 'No description available' fallback"
        );
    }

    #[test]
    fn is_meaningful_text_detects_placeholders() {
        use crate::models::is_meaningful_text;
        assert!(!is_meaningful_text(""));
        assert!(!is_meaningful_text("   "));
        assert!(!is_meaningful_text("No description available"));
        assert!(!is_meaningful_text("No description available."));
        assert!(!is_meaningful_text("no description available"));
        assert!(!is_meaningful_text("No description"));
        assert!(!is_meaningful_text("Please click the link for details."));
        assert!(is_meaningful_text(
            "Classes are suspended today due to weather."
        ));
        assert!(is_meaningful_text("Registration deadline extended."));
    }

    #[test]
    fn format_notice_message_respects_max_length() {
        let notice = Notice {
            title: "Length Test".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "a".repeat(5000),
            attachments: vec![],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(
            msg.chars().count() <= 4096,
            "message should respect Telegram max length, got {} chars",
            msg.chars().count()
        );
    }

    #[test]
    fn format_notice_message_surfaces_attachments_alongside_meaningful_body() {
        let notice = Notice {
            title: "Seat Plan for Final-Term Exams of Fall 2025-26 [ Only for LLB and BPharm ] ,Day 6 : Nov 22, 2026 ( Sunday )".to_string(),
            description: "Seat plan details".to_string(),
            url: "https://www.aiub.edu/seat-plan-for-final-term-exams-of-fall-2025-26--only-for-llb-and-bpharm--day-6--nov-22-2026--sunday-".to_string(),
            published_date: Some("2026-11-21".to_string()),
            body_text: "Please contact Exam Control Room if your name or course is not in the list. Annex 6 Exam Control Room # 6116 SLOT-1 (09:00 AM - 11:00 AM) SLOT-2 (12:00 PM - 02:00 PM)".to_string(),
            attachments: vec![
                "https://www.aiub.edu/Files/Uploads/day-6---slot-1(6).pdf".to_string(),
                "https://www.aiub.edu/Files/Uploads/day-6---slot-2(6).pdf".to_string(),
            ],
            detail_fetched: true,
        };

        let msg = format_notice_message(&notice, false);
        let fb_msg = format_facebook_message(&notice, false);

        // Crucial requirement: identical output across all platforms
        assert_eq!(
            msg, fb_msg,
            "Telegram and Facebook messages must be identical"
        );

        // Verify all parts are present in the exact unified structure
        assert!(msg.contains("Seat Plan for Final-Term Exams of Fall 2025-26"));
        assert!(msg.contains("Date: 21 Nov 2026"));
        assert!(msg.contains(
            "Please contact Exam Control Room if your name or course is not in the list."
        ));
        assert!(msg.contains("https://www.aiub.edu/Files/Uploads/day-6---slot-1(6).pdf"));
        assert!(msg.contains("https://www.aiub.edu/Files/Uploads/day-6---slot-2(6).pdf"));
        assert!(msg.ends_with("https://www.aiub.edu/seat-plan-for-final-term-exams-of-fall-2025-26--only-for-llb-and-bpharm--day-6--nov-22-2026--sunday-"));
    }

    #[test]
    fn format_notice_message_cross_platform_exact_equality() {
        let cases = vec![
            Notice {
                title: "Standard Text Only".to_string(),
                description: "Desc".to_string(),
                url: "https://www.aiub.edu/notice/text-only".to_string(),
                published_date: Some("2026-11-21".to_string()),
                body_text: "Plain announcement without attachments.".to_string(),
                attachments: vec![],
                detail_fetched: true,
            },
            Notice {
                title: "Flyer Banner Only".to_string(),
                description: "".to_string(),
                url: "https://www.aiub.edu/notice/flyer".to_string(),
                published_date: Some("2026-11-20".to_string()),
                body_text: "".to_string(),
                attachments: vec![
                    "https://www.aiub.edu/Files/Uploads/flyer.webp".to_string(),
                    "https://www.aiub.edu/Files/Uploads/info.pdf".to_string(),
                ],
                detail_fetched: true,
            },
            Notice {
                title: "Empty Notice".to_string(),
                description: "".to_string(),
                url: "https://www.aiub.edu/notice/empty".to_string(),
                published_date: None,
                body_text: "".to_string(),
                attachments: vec![],
                detail_fetched: false,
            },
        ];

        for notice in &cases {
            for edited in [false, true] {
                let tg_output = format_notice_message(notice, edited);
                let fb_output = format_facebook_message(notice, edited);
                assert_eq!(
                    tg_output, fb_output,
                    "Format mismatch between platforms for title='{}', edited={}",
                    notice.title, edited
                );
                assert!(
                    tg_output.chars().count() <= crate::config::UNIFIED_MAX_MESSAGE_LEN,
                    "Message exceeds unified limit of 4096"
                );
            }
        }
    }
}

#[cfg(test)]
mod notice_row_tests {
    use crate::models::NoticeRow;

    fn make_row(detail_checked_at: Option<&str>, detail_fetched_at: Option<&str>) -> NoticeRow {
        NoticeRow {
            title: "T".to_string(),
            description: "D".to_string(),
            link: "https://www.aiub.edu/notice/test".to_string(),
            published_date: None,
            body_text: "".to_string(),
            attachments_json: "[]".to_string(),
            content_hash: "abc".to_string(),
            tg_sent_at: None,
            tg_last_notified_hash: None,
            detail_checked_at: detail_checked_at.map(String::from),
            detail_fetched_at: detail_fetched_at.map(String::from),
            fb_sent_at: None,
            fb_last_notified_hash: None,
        }
    }

    #[test]
    fn detail_due_never_checked() {
        let row = make_row(None, None);
        assert!(
            row.detail_due("2026-01-01T00:00:00Z"),
            "never-checked should be due"
        );
    }

    #[test]
    fn detail_due_checked_but_not_fetched_before_cutoff() {
        let row = make_row(Some("2026-01-01T00:00:00Z"), None);
        assert!(
            row.detail_due("2026-01-01T01:00:00Z"),
            "checked before cutoff and not fetched should be due"
        );
    }

    #[test]
    fn detail_due_checked_but_not_fetched_after_cutoff() {
        let row = make_row(Some("2026-01-01T02:00:00Z"), None);
        assert!(
            !row.detail_due("2026-01-01T01:00:00Z"),
            "checked after cutoff should NOT be due (cooldown)"
        );
    }

    #[test]
    fn detail_due_already_fetched() {
        let row = make_row(Some("2026-01-01T00:00:00Z"), Some("2026-01-01T00:00:00Z"));
        // Checked == fetched an hour before the cutoff: due.
        assert!(row.detail_due("2026-01-01T01:00:00Z"));
    }

    // ── Audit #8: checked == fetched must respect the cooldown ───────

    #[test]
    fn detail_due_just_fetched_is_not_due() {
        // Cutoff 01:00 (= now 01:30 minus the 30-minute cooldown).
        // Checked == fetched at 01:25 — fetched 5 minutes ago, inside
        // the cooldown — must NOT be due. Before the fix the catch-all
        // arm returned true here unconditionally.
        let row = make_row(Some("2026-01-01T01:25:00Z"), Some("2026-01-01T01:25:00Z"));
        assert!(
            !row.detail_due("2026-01-01T01:00:00Z"),
            "row fetched 5 minutes ago must not be due"
        );
        // Same, with only the fetched timestamp present.
        let row = make_row(None, Some("2026-01-01T01:25:00Z"));
        assert!(
            !row.detail_due("2026-01-01T01:00:00Z"),
            "fetched-only row inside the cooldown must not be due"
        );
        // But once the last check is older than the cutoff, it is due.
        let row = make_row(Some("2026-01-01T00:30:00Z"), Some("2026-01-01T00:30:00Z"));
        assert!(
            row.detail_due("2026-01-01T01:00:00Z"),
            "row last checked before the cutoff is due"
        );
    }

    #[test]
    fn parsed_attachments_valid_json() {
        let row = NoticeRow {
            attachments_json: r#"["a.pdf","b.png"]"#.to_string(),
            ..make_row(None, None)
        };
        let attachments = row.parsed_attachments();
        assert_eq!(attachments, vec!["a.pdf", "b.png"]);
    }

    #[test]
    fn parsed_attachments_invalid_json() {
        let row = NoticeRow {
            attachments_json: "not valid json".to_string(),
            ..make_row(None, None)
        };
        let attachments = row.parsed_attachments();
        assert!(
            attachments.is_empty(),
            "invalid JSON should return empty vec"
        );
    }
}

#[cfg(test)]
mod lib_tests {
    use crate::{
        clean_mojibake, clean_mojibake_eq, format_detail_failure_alert, format_duration_human,
        format_listing_skip_message, format_outage_alert_message, format_recovery_alert_message,
        has_utf8_mojibake, titles_equivalent_clean,
    };
    use chrono::Duration;

    #[test]
    fn has_utf8_mojibake_detects_latin1_sequences() {
        // \u{00e2}\u{0080} is the typical Latin-1 mojibake prefix
        let mojibake = "2026\u{00e2}\u{0080}\u{0093}27";
        assert!(has_utf8_mojibake(mojibake));
    }

    #[test]
    fn has_utf8_mojibake_clean_string() {
        assert!(!has_utf8_mojibake("Normal text 2026–27"));
    }

    #[test]
    fn clean_mojibake_strips_sequences() {
        let mojibake = "2026\u{00e2}\u{0080}\u{0093}27";
        let clean = "202627";
        assert_eq!(clean_mojibake(mojibake), clean);
    }

    #[test]
    fn clean_mojibake_keeps_alphanumeric_lowercase() {
        assert_eq!(clean_mojibake("Hello World 123!"), "helloworld123");
    }

    #[test]
    fn titles_equivalent_clean_matches_despite_mojibake() {
        let clean = "Notice 2026–27";
        let mojibake = "Notice 2026\u{00e2}\u{0080}\u{0093}27";
        assert!(titles_equivalent_clean(clean, mojibake));
    }

    #[test]
    fn titles_equivalent_clean_exact_match() {
        assert!(titles_equivalent_clean("Same Title", "Same Title"));
    }

    #[test]
    fn titles_equivalent_clean_different() {
        assert!(!titles_equivalent_clean("Title A", "Title B"));
    }

    // L6: Bengali / accented text is alphanumeric, so a Bengali-only edit
    // must be detected (the old ASCII-only filter compared both sides as
    // empty strings and treated them as equal).
    #[test]
    fn clean_mojibake_keeps_bengali_alphanumeric() {
        // Bengali letters/digits survive cleaning (the virama in the
        // conjunct ক্ষ is a non-alphanumeric combining mark, so it is
        // stripped — the key property is that Bengali text is not wiped
        // out entirely, unlike the old ASCII-only filter).
        let cleaned = clean_mojibake("পরীক্ষার রুটিন ২০২৬");
        assert!(!cleaned.is_empty());
        assert!(
            cleaned.contains("২০২৬"),
            "Bengali digits must survive: {cleaned}"
        );
        assert!(
            cleaned.contains("টিন"),
            "Bengali letters must survive: {cleaned}"
        );
        assert_eq!(clean_mojibake("Café résumé!"), "caférésumé");
    }

    #[test]
    fn clean_mojibake_eq_detects_bengali_only_edit() {
        assert!(clean_mojibake_eq("পরীক্ষা", "পরীক্ষা!"));
        assert!(
            !clean_mojibake_eq("পরীক্ষা", "পরীক্ষার"),
            "a Bengali-only change must not compare equal"
        );
        assert!(!titles_equivalent_clean("পরীক্ষা", "পরীক্ষার"));
    }

    // M2: debounce helper — no timestamp / corrupt timestamp means due,
    // a recent timestamp means suppressed, an old one means due again.
    #[test]
    fn is_debounce_expired_handles_missing_corrupt_recent_and_old() {
        use crate::is_debounce_expired;
        let now = chrono::Utc::now();
        assert!(is_debounce_expired(None, now, 6));
        assert!(is_debounce_expired(Some("not-a-timestamp"), now, 6));
        let recent = (now - chrono::Duration::hours(1)).to_rfc3339();
        assert!(!is_debounce_expired(Some(&recent), now, 6));
        let old = (now - chrono::Duration::hours(7)).to_rfc3339();
        assert!(is_debounce_expired(Some(&old), now, 6));
    }

    #[test]
    fn format_detail_failure_alert_prefers_dom_when_parser_errors_dominate() {
        let diags = vec![
            "https://www.aiub.edu/notice1 (DOM: missing non-sticky question-column container, len=85000)".to_string(),
        ];
        let msg = format_detail_failure_alert(5, 1, 4, 0, 0, &diags);
        assert!(msg.contains("failed DOM validation"));
        assert!(msg.contains("DOM parse failed: 4"));
        assert!(msg.contains("HTTP/Network errors: 1"));
        assert!(msg.contains("notice1"));
    }

    #[test]
    fn format_detail_failure_alert_prefers_network_when_http_errors_dominate() {
        let diags = vec![
            "https://www.aiub.edu/notice1 (HTTP: HTTP 503 when fetching https://www.aiub.edu/notice1)".to_string(),
        ];
        let msg = format_detail_failure_alert(5, 5, 0, 0, 0, &diags);
        assert!(msg.contains("upstream AIUB server or network errors"));
        assert!(msg.contains("DOM parse failed: 0"));
        assert!(msg.contains("HTTP/Network errors: 5"));
        assert!(msg.contains("notice1"));
    }

    #[test]
    fn format_detail_failure_alert_cpu_limits() {
        let diags = vec![];
        let msg = format_detail_failure_alert(5, 0, 0, 0, 5, &diags);
        assert!(msg.contains("approached CPU limits"));
    }

    #[test]
    fn format_listing_skip_message_formats_correctly() {
        let timeout_err = "HTTP timeout after 15000ms fetching https://www.aiub.edu/category/notices?pageNo=1&pageSize=50";
        let msg = format_listing_skip_message(timeout_err);
        assert_eq!(
            msg,
            format!(
                "[Listing Fetch Skipped] AIUB listing page unreachable: {timeout_err}. Skipping this run."
            )
        );

        let status_err =
            "HTTP 502 when fetching https://www.aiub.edu/category/notices?pageNo=1&pageSize=50";
        let msg_status = format_listing_skip_message(status_err);
        assert!(msg_status.starts_with("[Listing Fetch Skipped]"));
        assert!(msg_status.contains("HTTP 502"));
    }

    #[test]
    fn format_duration_human_handles_various_ranges() {
        // Less than 1 minute
        assert_eq!(format_duration_human(Duration::seconds(0)), "< 1 minute");
        assert_eq!(format_duration_human(Duration::seconds(45)), "< 1 minute");

        // Minutes
        assert_eq!(format_duration_human(Duration::minutes(1)), "1 minute");
        assert_eq!(format_duration_human(Duration::minutes(2)), "2 minutes");
        assert_eq!(format_duration_human(Duration::minutes(59)), "59 minutes");

        // Exactly hours
        assert_eq!(format_duration_human(Duration::hours(1)), "1 hour");
        assert_eq!(format_duration_human(Duration::hours(2)), "2 hours");

        // Hours and minutes
        assert_eq!(
            format_duration_human(Duration::hours(1) + Duration::minutes(1)),
            "1 hour 1 minute"
        );
        assert_eq!(
            format_duration_human(Duration::hours(1) + Duration::minutes(15)),
            "1 hour 15 minutes"
        );
        assert_eq!(
            format_duration_human(Duration::hours(2) + Duration::minutes(30)),
            "2 hours 30 minutes"
        );

        // Days
        assert_eq!(format_duration_human(Duration::days(1)), "1 day");
        assert_eq!(
            format_duration_human(Duration::days(1) + Duration::minutes(30)),
            "1 day 30 minutes"
        );
        assert_eq!(
            format_duration_human(Duration::days(1) + Duration::hours(2) + Duration::minutes(15)),
            "1 day 2 hours 15 minutes"
        );
        assert_eq!(
            format_duration_human(Duration::days(3) + Duration::hours(5)),
            "3 days 5 hours"
        );
    }

    #[test]
    fn format_outage_alert_message_content() {
        let msg = format_outage_alert_message("1 hour");
        assert!(msg.contains("⚠️ AIUB Notice Checker"));
        assert!(msg.contains("unreachable for 1 hour"));
        assert!(msg.contains("Notice checks are paused"));
    }

    #[test]
    fn format_recovery_alert_message_content() {
        let msg = format_recovery_alert_message("2 hours 15 minutes");
        assert!(msg.contains("✅ AIUB Notice Checker"));
        assert!(msg.contains("back online"));
        assert!(msg.contains("total outage of 2 hours 15 minutes"));
        assert!(msg.contains("Notice checking has resumed"));
    }
}

// M15: tests for the pure decision functions extracted from run() —
// detail-target selection (Priority A/B/C, incl. the M7 cooldown gate),
// per-platform notification eligibility, and silent-sync eligibility
// (incl. the M6 listing-title-only rule).
#[cfg(test)]
mod run_decision_tests {
    use crate::models::{Notice, NoticeRow};
    use crate::{
        dispatch_deadline_exceeded, platform_notification, select_detail_targets, silent_sync_flags,
    };
    use std::collections::HashMap;

    fn notice(url: &str, title: &str) -> Notice {
        Notice {
            title: title.to_string(),
            description: "Desc".to_string(),
            url: url.to_string(),
            published_date: None,
            body_text: String::new(),
            attachments: vec![],
            detail_fetched: false,
        }
    }

    fn row(link: &str, title: &str) -> NoticeRow {
        NoticeRow {
            title: title.to_string(),
            description: "Desc".to_string(),
            link: link.to_string(),
            published_date: None,
            body_text: String::new(),
            attachments_json: "[]".to_string(),
            content_hash: "hash1".to_string(),
            tg_sent_at: None,
            tg_last_notified_hash: None,
            detail_checked_at: None,
            detail_fetched_at: None,
            fb_sent_at: None,
            fb_last_notified_hash: None,
        }
    }

    fn fetched_notice(url: &str, title: &str) -> Notice {
        let mut n = notice(url, title);
        n.detail_fetched = true;
        n
    }

    // ── Audit #2: dispatch-deadline buffer ───────────────────────────

    #[test]
    fn dispatch_deadline_exceeded_only_when_buffer_gone() {
        // 240s deadline, 30s buffer: dispatch may start up to 210s in.
        assert!(!dispatch_deadline_exceeded(0));
        assert!(!dispatch_deadline_exceeded(209_999));
        assert!(dispatch_deadline_exceeded(210_000));
        assert!(dispatch_deadline_exceeded(300_000));
    }

    // ── Priority A: brand-new notices ────────────────────────────────

    #[test]
    fn select_detail_targets_picks_new_notices_first() {
        let listing = vec![
            notice("https://www.aiub.edu/notice/1", "New One"),
            notice("https://www.aiub.edu/notice/2", "New Two"),
        ];
        let existing = HashMap::new();
        let sel = select_detail_targets(&listing, &existing, "2026-11-21T12:00:00Z");
        assert_eq!(sel.new_count, 2);
        assert_eq!(sel.backfill_count, 0);
        assert_eq!(sel.rotation_count, 0);
        assert_eq!(
            sel.targets,
            vec![
                "https://www.aiub.edu/notice/1".to_string(),
                "https://www.aiub.edu/notice/2".to_string(),
            ]
        );
    }

    // ── Priority B + M7: mojibake backfill gated on the retry cooldown ─

    #[test]
    fn select_detail_targets_mojibake_backfill_respects_cooldown() {
        // Row already fetched, but its stored title has mojibake and it
        // was checked 15 minutes ago — inside the 30-minute cooldown, so
        // it must NOT be re-fetched on this run (M7).
        let listing = vec![notice("https://www.aiub.edu/notice/1", "Clean Title")];
        let mut existing = HashMap::new();
        let mut r = row(
            "https://www.aiub.edu/notice/1",
            "2026\u{00e2}\u{0080}\u{0093}27 Title",
        );
        r.detail_fetched_at = Some("2026-11-21T11:00:00Z".to_string());
        r.detail_checked_at = Some("2026-11-21T12:15:00Z".to_string());
        existing.insert(r.link.clone(), r);

        let sel = select_detail_targets(&listing, &existing, "2026-11-21T12:00:00Z");
        assert_eq!(
            sel.backfill_count, 0,
            "mojibake backfill inside the cooldown must not be selected via Priority B (M7)"
        );

        // Same row, but last checked before the cutoff — now it is due.
        let mut r2 = row(
            "https://www.aiub.edu/notice/1",
            "2026\u{00e2}\u{0080}\u{0093}27 Title",
        );
        r2.detail_fetched_at = Some("2026-11-21T11:00:00Z".to_string());
        r2.detail_checked_at = Some("2026-11-21T11:30:00Z".to_string());
        let mut existing2 = HashMap::new();
        existing2.insert(r2.link.clone(), r2);
        let sel2 = select_detail_targets(&listing, &existing2, "2026-11-21T12:00:00Z");
        assert_eq!(sel2.backfill_count, 1);
        assert_eq!(sel2.targets.len(), 1);
    }

    #[test]
    fn select_detail_targets_listing_change_respects_cooldown() {
        // Listing title genuinely changed, but the row was checked 15
        // minutes ago — the change waits for the cooldown (M7).
        let listing = vec![notice(
            "https://www.aiub.edu/notice/1",
            "Updated Title Here",
        )];
        let mut existing = HashMap::new();
        let mut r = row("https://www.aiub.edu/notice/1", "Original Title");
        r.detail_fetched_at = Some("2026-11-21T11:00:00Z".to_string());
        r.detail_checked_at = Some("2026-11-21T12:15:00Z".to_string());
        existing.insert(r.link.clone(), r);

        let sel = select_detail_targets(&listing, &existing, "2026-11-21T12:00:00Z");
        assert_eq!(
            sel.backfill_count, 0,
            "listing-change backfill inside the cooldown must not be selected via Priority B (M7)"
        );
    }

    // ── Priority C: rolling edit check, oldest first ─────────────────

    #[test]
    fn select_detail_targets_rotation_picks_oldest_checked_first() {
        let listing = vec![
            notice("https://www.aiub.edu/notice/1", "One"),
            notice("https://www.aiub.edu/notice/2", "Two"),
        ];
        let mut existing = HashMap::new();
        for (link, title, checked) in [
            (
                "https://www.aiub.edu/notice/1",
                "One",
                "2026-11-21T10:00:00Z",
            ),
            (
                "https://www.aiub.edu/notice/2",
                "Two",
                "2026-11-21T09:00:00Z",
            ),
        ] {
            let mut r = row(link, title);
            r.detail_fetched_at = Some("2026-11-21T08:00:00Z".to_string());
            r.detail_checked_at = Some(checked.to_string());
            existing.insert(r.link.clone(), r);
        }
        let sel = select_detail_targets(&listing, &existing, "2026-11-21T12:00:00Z");
        assert_eq!(sel.rotation_count, 2);
        assert_eq!(
            sel.targets,
            vec![
                "https://www.aiub.edu/notice/2".to_string(),
                "https://www.aiub.edu/notice/1".to_string(),
            ],
            "oldest detail_checked_at first"
        );
    }

    // ── Notification eligibility ─────────────────────────────────────

    #[test]
    fn platform_notification_never_sent_queues_new() {
        assert_eq!(
            platform_notification(None, None, true, true, false, "hash2"),
            Some(false)
        );
        // Not attempted and never checked: nothing to queue.
        assert_eq!(
            platform_notification(None, None, false, false, false, "hash2"),
            None
        );
        // Attempted but detail failed still queues (matches run()).
        assert_eq!(
            platform_notification(None, None, false, true, false, "hash2"),
            Some(false)
        );
    }

    #[test]
    fn platform_notification_sent_only_queues_real_edits() {
        let sent_at = Some("2026-11-21T10:00:00Z");
        assert_eq!(
            platform_notification(sent_at, Some("hash1"), true, true, true, "hash2"),
            Some(true),
            "hash changed after a detail attempt is an edit"
        );
        assert_eq!(
            platform_notification(sent_at, Some("hash1"), true, true, true, "hash1"),
            None,
            "unchanged hash is not an edit"
        );
        assert_eq!(
            platform_notification(sent_at, Some("hash1"), true, false, true, "hash2"),
            None,
            "no detail attempt, no edit check"
        );
    }

    // ── Silent sync (M6) ─────────────────────────────────────────────

    #[test]
    fn silent_sync_flags_full_detail_title_still_syncs() {
        // M6: the listing title is truncated ("..."), the detail fetch
        // returned the full title. The old check required the full
        // detail title to be equivalent to the cached title too, which
        // defeated the sync and produced a false [EDITED]. The listing
        // title alone decides now.
        let r = {
            let mut r = row(
                "https://www.aiub.edu/notice/1",
                "Important Notice About Examination Schedule",
            );
            r.tg_sent_at = Some("2026-11-21T10:00:00Z".to_string());
            r.tg_last_notified_hash = Some("hash1".to_string());
            r
        };
        let item = fetched_notice(
            "https://www.aiub.edu/notice/1",
            "Important Notice About Examination Schedule",
        );
        let (tg, fb) = silent_sync_flags(
            &r,
            "Important Notice About Exam...",
            &item,
            "hash2",
            &item.attachments_json(),
            false,
        );
        assert!(
            tg,
            "first-detail backfill with a truncated listing title must sync (M6)"
        );
        assert!(!fb, "Facebook never sent, so it has nothing to sync");
    }

    #[test]
    fn silent_sync_flags_never_sent_platform_does_not_sync() {
        // H1 (preserved): a platform that never sent must never be
        // silently synced — only already-sent platforms sync their hash.
        let r = {
            let mut r = row("https://www.aiub.edu/notice/1", "Some Title");
            r.fb_sent_at = Some("2026-11-21T10:00:00Z".to_string());
            r.fb_last_notified_hash = Some("hash1".to_string());
            r
        };
        let item = fetched_notice("https://www.aiub.edu/notice/1", "Some Title");
        let (tg, fb) = silent_sync_flags(
            &r,
            "Some Title",
            &item,
            "hash2",
            &item.attachments_json(),
            false,
        );
        assert!(!tg, "Telegram never sent");
        assert!(fb, "Facebook sent and hash differs");
    }

    #[test]
    fn silent_sync_flags_genuine_listing_change_does_not_sync() {
        let r = {
            let mut r = row("https://www.aiub.edu/notice/1", "Old Title");
            r.tg_sent_at = Some("2026-11-21T10:00:00Z".to_string());
            r.tg_last_notified_hash = Some("hash1".to_string());
            r
        };
        let item = fetched_notice("https://www.aiub.edu/notice/1", "Brand New Title");
        let (tg, fb) = silent_sync_flags(
            &r,
            "Brand New Title",
            &item,
            "hash2",
            &item.attachments_json(),
            false,
        );
        assert!(
            !tg && !fb,
            "a genuine listing change is an edit, not a silent sync"
        );
    }
}

#[cfg(test)]
mod config_tests {
    use crate::config::*;

    #[test]
    fn base_url_is_https() {
        assert!(BASE_URL.starts_with("https://"));
    }

    #[test]
    fn base_host_matches_base_url() {
        assert!(BASE_URL.contains(BASE_HOST));
    }

    #[test]
    fn outage_config_constants() {
        assert_eq!(AIUB_OUTAGE_ALERT_THRESHOLD_MINS, 60);
        assert_eq!(AIUB_OUTAGE_DOWN_SINCE_KEY, "aiub_down_since");
        assert_eq!(AIUB_OUTAGE_ALERTED_KEY, "aiub_outage_alerted");
    }

    #[test]
    fn notice_path_starts_with_slash() {
        assert!(NOTICE_PATH.starts_with('/'));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn sensible_config_ranges() {
        assert!(LIST_PAGE_SIZE > 0 && LIST_PAGE_SIZE <= 200);
        assert!(MAX_DETAIL_FETCHES_PER_RUN > 0 && MAX_DETAIL_FETCHES_PER_RUN <= 50);
        assert!(MAX_TELEGRAM_SENDS_PER_RUN > 0);
        assert!(HTTP_TIMEOUT_MS >= 1000);
        assert!(MAX_REDIRECTS >= 1);
        assert!(MAX_URL_LENGTH > 100);
        assert!(MAX_HTML_BYTES > 1024);
        assert!((0.0..=1.0).contains(&MIN_LISTING_DATE_COVERAGE));
        assert!((0.0..=1.0).contains(&MIN_LISTING_DESC_COVERAGE));
        assert!((0.0..=1.0).contains(&DETAIL_FAILURE_RATIO));

        // Facebook configuration checks
        assert_eq!(FACEBOOK_GRAPH_API_VERSION, "v27.0");
        assert!(MAX_FACEBOOK_SENDS_PER_RUN > 0 && MAX_FACEBOOK_SENDS_PER_RUN <= 10);
        assert!(FACEBOOK_PACE_MS >= 1000);
        assert!(FACEBOOK_BUC_USAGE_THRESHOLD_PCT >= 50 && FACEBOOK_BUC_USAGE_THRESHOLD_PCT <= 100);
        assert!(FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS > 0);
        assert!(FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS > 0);
        assert!(FACEBOOK_ADMIN_ALERT_DEBOUNCE_HOURS > 0);
    }
}

#[cfg(test)]
mod facebook_tests {
    use crate::config::{FacebookSecrets, UNIFIED_MAX_MESSAGE_LEN};
    use crate::facebook::{
        MetaErrorAction, classify_meta_error, parse_buc_usage_header, parse_post_id,
        stored_time_is_after,
    };
    use crate::models::{Notice, format_facebook_message};

    #[test]
    fn stored_time_is_after_handles_future_past_and_corrupt() {
        // M2: a corrupt stored timestamp must be distinguishable from an
        // active one — under the old string comparison, "zzzz" sorted
        // after every RFC3339 string and suppressed posting forever.
        use chrono::{Duration, Utc};
        let now = Utc::now();
        assert_eq!(
            stored_time_is_after(&(now + Duration::hours(1)).to_rfc3339(), now),
            Some(true)
        );
        assert_eq!(
            stored_time_is_after(&(now - Duration::hours(1)).to_rfc3339(), now),
            Some(false)
        );
        assert_eq!(stored_time_is_after("not-a-timestamp", now), None);
        assert_eq!(stored_time_is_after("zzzz", now), None);
        assert_eq!(stored_time_is_after("", now), None);
    }

    #[test]
    fn format_facebook_message_standard_text_notice() {
        let notice = Notice {
            title: "Midterm Exam Schedule".to_string(),
            description: "Listing description".to_string(),
            url: "https://www.aiub.edu/notice/midterm-schedule".to_string(),
            published_date: Some("2026-11-15".to_string()),
            body_text: "All midterm exams will be held as per the published routine.".to_string(),
            attachments: vec![],
            detail_fetched: true,
        };

        let msg = format_facebook_message(&notice, false);
        assert!(!msg.contains("[EDITED]"));
        assert!(msg.starts_with("Midterm Exam Schedule\n\nDate: 15 Nov 2026\n\n"));
        assert!(msg.contains("All midterm exams will be held as per the published routine."));
        assert!(msg.ends_with("\n\nhttps://www.aiub.edu/notice/midterm-schedule"));
        // Confirm zero HTML or markdown formatting leaks
        assert!(!msg.contains('<') && !msg.contains('>'));
        assert!(!msg.contains("**") && !msg.contains("__"));
    }

    #[test]
    fn format_facebook_message_image_only_flyer_notice() {
        let notice = Notice {
            title: "Campus Job Fair 2026".to_string(),
            description: "No description available".to_string(),
            url: "https://www.aiub.edu/notice/job-fair-2026".to_string(),
            published_date: Some("2026-11-20".to_string()),
            body_text: "".to_string(),
            attachments: vec![
                "https://www.aiub.edu/Files/Uploads/webp/job-fair-banner.webp".to_string(),
                "https://www.aiub.edu/Files/Uploads/job-fair-schedule.pdf".to_string(),
            ],
            detail_fetched: true,
        };

        let msg = format_facebook_message(&notice, false);
        assert!(!msg.contains("No description available"));
        assert!(!msg.contains("Please click the link for details."));
        // Both attachments should appear on their own lines
        assert!(msg.contains("https://www.aiub.edu/Files/Uploads/webp/job-fair-banner.webp"));
        assert!(msg.contains("https://www.aiub.edu/Files/Uploads/job-fair-schedule.pdf"));
        assert!(msg.ends_with("https://www.aiub.edu/notice/job-fair-2026"));
    }

    #[test]
    fn format_facebook_message_edited_notice_alert() {
        let notice = Notice {
            title: "Revised Class Schedule".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/revised-schedule".to_string(),
            published_date: Some("2026-11-18".to_string()),
            body_text: "Room allocations updated for Annex 3.".to_string(),
            attachments: vec![],
            detail_fetched: true,
        };

        let msg = format_facebook_message(&notice, true);
        assert!(msg.starts_with("[EDITED] Revised Class Schedule"));
    }

    #[test]
    fn format_facebook_message_protects_clickable_links_spacing() {
        let notice = Notice {
            title: "Link Spacing Test".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/link-test".to_string(),
            published_date: None,
            body_text: "Body text.".to_string(),
            attachments: vec!["https://www.aiub.edu/flyer.jpg".to_string()],
            detail_fetched: true,
        };

        let msg = format_facebook_message(&notice, false);
        // Ensure no punctuation or bracket is attached to the URLs
        assert!(!msg.contains("https://www.aiub.edu/notice/link-test."));
        assert!(!msg.contains("https://www.aiub.edu/notice/link-test)"));
        assert!(!msg.contains("https://www.aiub.edu/flyer.jpg."));
        assert!(!msg.contains("https://www.aiub.edu/flyer.jpg)"));
    }

    #[test]
    fn format_facebook_message_defensive_length_cap_preserves_urls() {
        let notice = Notice {
            title: "Very Long Notice".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/long-notice".to_string(),
            published_date: Some("2026-11-19".to_string()),
            body_text: "Lorem ipsum dolor sit amet. ".repeat(300),
            attachments: vec!["https://www.aiub.edu/uploads/flyer.webp".to_string()],
            detail_fetched: true,
        };

        let msg = format_facebook_message(&notice, false);
        assert!(
            msg.chars().count() <= UNIFIED_MAX_MESSAGE_LEN,
            "Message length {} exceeds max {}",
            msg.chars().count(),
            UNIFIED_MAX_MESSAGE_LEN
        );
        // Ellipsis appended to truncated body
        assert!(msg.contains("..."));
        // Attachment and notice URLs preserved untruncated
        assert!(msg.contains("https://www.aiub.edu/uploads/flyer.webp"));
        assert!(msg.ends_with("https://www.aiub.edu/notice/long-notice"));
    }

    #[test]
    fn format_facebook_message_empty_body_fallback() {
        let notice = Notice {
            title: "Empty Notice".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/empty".to_string(),
            published_date: None,
            body_text: "".to_string(),
            attachments: vec![],
            detail_fetched: false,
        };

        let msg = format_facebook_message(&notice, false);
        assert!(msg.contains("Please click the link for details."));
        assert!(msg.ends_with("https://www.aiub.edu/notice/empty"));
    }

    // ── Meta rate-limit header parsing ───────────────────────────────

    #[test]
    fn parse_buc_usage_header_valid_pages_format() {
        let header = r#"{"1234567890":[{"type":"pages","call_count":45,"total_cputime":30,"total_time":20,"estimated_time_to_regain_access":0}]}"#;
        let usage = parse_buc_usage_header(header).expect("valid BUC header");
        assert_eq!(usage.call_count, 45);
        assert_eq!(usage.total_cputime, 30);
        assert_eq!(usage.total_time, 20);
        assert_eq!(usage.estimated_time_to_regain_access, 0);
        assert_eq!(usage.max_usage(), 45);
        assert!(!usage.is_approaching_limit());
    }

    #[test]
    fn parse_buc_usage_header_approaching_limit() {
        let header = r#"{"1234567890":[{"type":"pages","call_count":82,"total_cputime":50,"total_time":40,"estimated_time_to_regain_access":15}]}"#;
        let usage = parse_buc_usage_header(header).expect("valid BUC header");
        assert!(usage.is_approaching_limit());
        assert_eq!(usage.max_usage(), 82);
        assert_eq!(usage.estimated_time_to_regain_access, 15);
    }

    #[test]
    fn parse_buc_usage_header_flat_app_usage() {
        let header = r#"{"call_count":85,"total_cputime":10,"total_time":15}"#;
        let usage = parse_buc_usage_header(header).expect("valid app usage");
        assert_eq!(usage.call_count, 85);
        assert!(usage.is_approaching_limit());
        assert_eq!(usage.max_usage(), 85);
    }

    // ── Audit #14: aggregate across all BUC entries ──────────────────

    #[test]
    fn parse_buc_usage_header_aggregates_multiple_entries() {
        // Two buckets: the first is cool, the second is hot with a
        // 120-minute regain estimate. The max of each field wins —
        // the first entry must not mask the hot bucket.
        let header = r#"{"111":[{"type":"pages","call_count":45,"total_cputime":30,"total_time":20,"estimated_time_to_regain_access":0}],"222":[{"type":"pages","call_count":95,"total_cputime":10,"total_time":10,"estimated_time_to_regain_access":120}]}"#;
        let usage = parse_buc_usage_header(header).expect("valid BUC header");
        assert_eq!(usage.call_count, 95);
        assert_eq!(usage.total_cputime, 30);
        assert_eq!(usage.total_time, 20);
        assert_eq!(usage.estimated_time_to_regain_access, 120);
        assert!(usage.is_approaching_limit());
        assert_eq!(usage.max_usage(), 95);
    }

    // ── Audit #3: a 2xx without a post id is not proof of posting ────

    #[test]
    fn parse_post_id_requires_non_empty_id() {
        assert_eq!(
            parse_post_id(r#"{"id":"123_456"}"#).as_deref(),
            Some("123_456")
        );
        assert_eq!(parse_post_id("{}"), None, "missing id");
        assert_eq!(parse_post_id(r#"{"id":""}"#), None, "empty id");
        assert_eq!(parse_post_id(r#"{"id":null}"#), None, "null id");
        assert_eq!(
            parse_post_id("<html>error page</html>"),
            None,
            "unparsable body"
        );
    }

    #[test]
    fn parse_buc_usage_header_malformed() {
        assert!(parse_buc_usage_header("").is_none());
        assert!(parse_buc_usage_header("not json").is_none());
        assert!(parse_buc_usage_header("[]").is_none());
        assert!(parse_buc_usage_header("{}").is_none());
    }

    #[test]
    fn parse_buc_usage_header_caps_regain_access_at_seven_days() {
        // Medium #6: the header value is untrusted input — an absurd
        // estimated_time_to_regain_access must not push the cooldown
        // arbitrarily far into the future.
        let header = r#"{"1234567890":[{"type":"pages","call_count":99,"total_cputime":99,"total_time":99,"estimated_time_to_regain_access":999999}]}"#;
        let usage = parse_buc_usage_header(header).expect("valid BUC header");
        assert_eq!(
            usage.estimated_time_to_regain_access,
            crate::config::FACEBOOK_BUC_MAX_COOLDOWN_MINS
        );
        assert_eq!(usage.estimated_time_to_regain_access, 60 * 24 * 7);
    }

    #[test]
    fn facebook_secrets_debug_redacts_token() {
        // Medium #9: the hand-written Debug must never print the token.
        let secrets = FacebookSecrets::from_raw(Some("1234567890"), Some("EAABsekrit-token-value"))
            .expect("valid secrets");
        let rendered = format!("{secrets:?}");
        assert!(rendered.contains("1234567890"));
        assert!(!rendered.contains("EAABsekrit-token-value"));
        assert!(rendered.contains("[REDACTED]"));
    }

    // ── Meta error classification ────────────────────────────────────

    #[test]
    fn classify_rate_limit_error_codes() {
        for code in [80001, 32, 4, 17, 341, 613] {
            assert_eq!(
                classify_meta_error(code, None),
                MetaErrorAction::RateLimitCooldown {
                    estimated_minutes: 0
                },
                "Code {code} should be classified as RateLimitCooldown"
            );
        }
    }

    #[test]
    fn classify_policy_spam_block_code_368() {
        assert_eq!(classify_meta_error(368, None), MetaErrorAction::SpamBlock);
    }

    #[test]
    fn classify_duplicate_post_code_506() {
        assert_eq!(
            classify_meta_error(506, None),
            MetaErrorAction::DuplicatePost
        );
    }

    #[test]
    fn classify_auth_failure_codes() {
        assert_eq!(
            classify_meta_error(190, Some(458)),
            MetaErrorAction::AuthFailure {
                code: 190,
                subcode: Some(458)
            }
        );
        assert_eq!(
            classify_meta_error(102, Some(463)),
            MetaErrorAction::AuthFailure {
                code: 102,
                subcode: Some(463)
            }
        );
    }

    #[test]
    fn classify_permission_error_codes() {
        assert_eq!(
            classify_meta_error(10, None),
            MetaErrorAction::PermissionError { code: 10 }
        );
        for code in 200..=299 {
            assert_eq!(
                classify_meta_error(code, None),
                MetaErrorAction::PermissionError { code }
            );
        }
    }

    #[test]
    fn classify_transient_error_codes() {
        assert_eq!(
            classify_meta_error(1, None),
            MetaErrorAction::TransientError
        );
        assert_eq!(
            classify_meta_error(2, None),
            MetaErrorAction::TransientError
        );
    }

    #[test]
    fn classify_unknown_error_code() {
        assert_eq!(
            classify_meta_error(99999, None),
            MetaErrorAction::Unknown { code: 99999 }
        );
    }

    // H2: a bare 401 with no parseable Meta error is transient, not an
    // auth failure — it must never permanently pause Facebook posting.
    // A 401 carrying a real Meta 190 error is still an auth failure.
    #[test]
    fn classify_publish_error_bare_401_is_transient() {
        use crate::facebook::{FacebookPostResult, classify_publish_error};
        let result = classify_publish_error(
            401,
            0,
            None,
            "Unknown Meta API error",
            "<html>401</html>",
            None,
        );
        assert!(
            matches!(result, FacebookPostResult::TransientFailure { .. }),
            "bare 401 must be transient, got {result:?}"
        );
        let result =
            classify_publish_error(401, 190, Some(458), "Invalid OAuth access token", "", None);
        assert!(
            matches!(result, FacebookPostResult::AuthError { .. }),
            "401 with Meta code 190 must be an auth failure, got {result:?}"
        );
    }

    // L4: a bare 403 with no parseable Meta error is an auth/permission
    // error, matching the parsed permission-error handling.
    #[test]
    fn classify_publish_error_bare_403_is_auth_error() {
        use crate::facebook::{FacebookPostResult, classify_publish_error};
        let result =
            classify_publish_error(403, 0, None, "Unknown Meta API error", "Forbidden", None);
        assert!(
            matches!(result, FacebookPostResult::AuthError { .. }),
            "bare 403 must be an auth error, got {result:?}"
        );
    }

    // H3: a bare HTTP 429 is a rate limit even with no parseable Meta
    // error payload — it must set a cooldown (via the RateLimited arm in
    // lib.rs) instead of falling through to Unknown with no cooldown.
    #[test]
    fn classify_publish_error_bare_429_is_rate_limited() {
        use crate::config::FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS;
        use crate::facebook::{BucUsage, FacebookPostResult, classify_publish_error};

        // No BUC usage: fall back to the 60-minute cooldown.
        let result =
            classify_publish_error(429, 0, None, "Too Many Requests", "rate limited", None);
        assert!(
            matches!(
                result,
                FacebookPostResult::RateLimited {
                    cooldown_minutes
                } if cooldown_minutes == FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS
            ),
            "bare 429 with no BUC usage must be RateLimited with the fallback, got {result:?}"
        );

        // BUC usage with an estimated regain time: use that instead.
        let usage = BucUsage {
            call_count: 95,
            total_cputime: 10,
            total_time: 10,
            estimated_time_to_regain_access: 15,
        };
        let result = classify_publish_error(
            429,
            0,
            None,
            "Too Many Requests",
            "rate limited",
            Some(&usage),
        );
        assert!(
            matches!(
                result,
                FacebookPostResult::RateLimited {
                    cooldown_minutes: 15
                }
            ),
            "bare 429 with BUC regain time must use it, got {result:?}"
        );

        // A 429 carrying a parsed rate-limit code is rate-limited too.
        let result =
            classify_publish_error(429, 4, None, "Application request limit reached", "", None);
        assert!(
            matches!(result, FacebookPostResult::RateLimited { .. }),
            "429 with Meta code 4 must be RateLimited, got {result:?}"
        );
    }

    // ── Facebook secrets & graceful feature gating ───────────────────

    #[test]
    fn facebook_secrets_from_raw_handles_missing_or_empty() {
        use crate::config::FacebookSecrets;

        assert_eq!(FacebookSecrets::from_raw(None, None), None);
        assert_eq!(FacebookSecrets::from_raw(Some("123"), None), None);
        assert_eq!(FacebookSecrets::from_raw(None, Some("token")), None);
        assert_eq!(FacebookSecrets::from_raw(Some(""), Some("token")), None);
        assert_eq!(FacebookSecrets::from_raw(Some("   "), Some("token")), None);
        assert_eq!(FacebookSecrets::from_raw(Some("123"), Some("")), None);
        assert_eq!(FacebookSecrets::from_raw(Some("123"), Some("   ")), None);

        let valid = FacebookSecrets::from_raw(Some(" 1234567890 "), Some(" EAAB...token "));
        assert_eq!(
            valid,
            Some(FacebookSecrets {
                page_id: "1234567890".to_string(),
                page_access_token: "EAAB...token".to_string(),
            })
        );
    }

    // ── D1 schema backfill & cold-start queueing behavior ────────────

    #[test]
    fn migration_sql_contains_backfill_and_column_renames() {
        let migration_sql = include_str!("../migrations/0002_facebook_and_tg_rename.sql");

        // Verify Telegram column renames
        assert!(migration_sql.contains("ALTER TABLE notices RENAME COLUMN sent_at TO tg_sent_at;"));
        assert!(migration_sql.contains(
            "ALTER TABLE notices RENAME COLUMN last_notified_hash TO tg_last_notified_hash;"
        ));
        assert!(migration_sql.contains("ALTER TABLE notices RENAME COLUMN notification_attempted_at TO tg_notification_attempted_at;"));

        // Verify Facebook column additions
        assert!(migration_sql.contains("ALTER TABLE notices ADD COLUMN fb_sent_at TEXT;"));
        assert!(
            migration_sql.contains("ALTER TABLE notices ADD COLUMN fb_last_notified_hash TEXT;")
        );
        assert!(
            migration_sql
                .contains("ALTER TABLE notices ADD COLUMN fb_notification_attempted_at TEXT;")
        );
        assert!(migration_sql.contains("ALTER TABLE notices ADD COLUMN fb_post_id TEXT;"));

        // Verify historical backfill preventing archive flood
        assert!(migration_sql.contains("UPDATE notices"));
        assert!(migration_sql.contains("SET fb_sent_at = tg_sent_at,"));
        assert!(migration_sql.contains("fb_last_notified_hash = tg_last_notified_hash"));
        assert!(migration_sql.contains("WHERE tg_sent_at IS NOT NULL;"));
    }

    #[test]
    fn independent_dispatch_queueing_behavior() {
        use crate::models::NoticeRow;

        let make_test_row = |tg_sent: Option<&str>,
                             fb_sent: Option<&str>,
                             tg_hash: Option<&str>,
                             fb_hash: Option<&str>| {
            NoticeRow {
                title: "Test Notice".to_string(),
                description: "Test Desc".to_string(),
                link: "https://www.aiub.edu/notice/1".to_string(),
                published_date: Some("2026-11-20".to_string()),
                body_text: "Body".to_string(),
                attachments_json: "[]".to_string(),
                content_hash: "hash_v1".to_string(),
                tg_sent_at: tg_sent.map(String::from),
                tg_last_notified_hash: tg_hash.map(String::from),
                detail_checked_at: Some("2026-11-20T00:00:00Z".to_string()),
                detail_fetched_at: Some("2026-11-20T00:00:00Z".to_string()),
                fb_sent_at: fb_sent.map(String::from),
                fb_last_notified_hash: fb_hash.map(String::from),
            }
        };

        // Case 1: Backfilled historical row — neither Telegram nor Facebook should dispatch
        let row_backfilled = make_test_row(
            Some("2026-11-19T00:00:00Z"),
            Some("2026-11-19T00:00:00Z"),
            Some("hash_v1"),
            Some("hash_v1"),
        );
        let current_hash = "hash_v1";
        let tg_needs = row_backfilled.tg_sent_at.is_none()
            || row_backfilled.tg_last_notified_hash.as_deref() != Some(current_hash);
        let fb_needs = row_backfilled.fb_sent_at.is_none()
            || row_backfilled.fb_last_notified_hash.as_deref() != Some(current_hash);
        assert!(!tg_needs, "Backfilled row must not trigger Telegram send");
        assert!(!fb_needs, "Backfilled row must not trigger Facebook send");

        // Case 2: Telegram succeeded, Facebook failed/paused previously — only Facebook should dispatch
        let row_tg_only = make_test_row(Some("2026-11-20T00:00:00Z"), None, Some("hash_v1"), None);
        let tg_needs = row_tg_only.tg_sent_at.is_none();
        let fb_needs = row_tg_only.fb_sent_at.is_none();
        assert!(!tg_needs, "Telegram already sent, must not re-send");
        assert!(fb_needs, "Facebook has not sent, must dispatch");

        // Case 3: Notice content changed (hash_v2) — both should dispatch as [EDITED]
        let current_hash_v2 = "hash_v2";
        let tg_edited = row_backfilled.tg_last_notified_hash.as_deref() != Some(current_hash_v2);
        let fb_edited = row_backfilled.fb_last_notified_hash.as_deref() != Some(current_hash_v2);
        assert!(tg_edited, "Telegram must trigger for edited notice");
        assert!(fb_edited, "Facebook must trigger for edited notice");
    }
}

#[cfg(test)]
mod db_tests {
    #[test]
    fn d1_metrics_default_has_zero_failed_statements() {
        // M8: execute_batch reports per-statement failures through
        // D1Metrics.failed_statements so callers can tell a partial
        // batch from a clean one.
        use crate::models::D1Metrics;
        let m = D1Metrics::default();
        assert_eq!(m.failed_statements, 0);
        assert_eq!(m.rows_read, 0);
        assert_eq!(m.rows_written, 0);
    }

    // ── Audit #15: the three upsert branches, behaviourally ─────────
    //
    // One SQLite test per branch of build_upsert_statement() (via
    // upsert_statement_sql()), executing the mirror SQL against a real
    // in-memory SQLite database — a one-placeholder slip in a
    // 12-column upsert used to compile fine and only fail live.

    fn upsert_test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        conn.execute_batch(
            "CREATE TABLE notices (
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                link TEXT PRIMARY KEY,
                published_date TEXT,
                body_text TEXT NOT NULL DEFAULT '',
                attachments_json TEXT NOT NULL DEFAULT '[]',
                content_hash TEXT NOT NULL DEFAULT '',
                first_seen_at TEXT NOT NULL DEFAULT '',
                last_seen_at TEXT NOT NULL DEFAULT '',
                detail_checked_at TEXT,
                detail_fetched_at TEXT,
                content_changed_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .expect("create notices table");
        conn
    }

    struct UpsertRow {
        title: String,
        body_text: String,
        attachments_json: String,
        content_hash: String,
        published_date: Option<String>,
        last_seen_at: String,
        detail_checked_at: Option<String>,
        detail_fetched_at: Option<String>,
        content_changed_at: String,
    }

    fn read_upsert_row(conn: &rusqlite::Connection, link: &str) -> UpsertRow {
        conn.query_row(
            "SELECT title, body_text, attachments_json, content_hash, published_date, \
             last_seen_at, detail_checked_at, detail_fetched_at, content_changed_at \
             FROM notices WHERE link = ?1",
            rusqlite::params![link],
            |row| {
                Ok(UpsertRow {
                    title: row.get(0)?,
                    body_text: row.get(1)?,
                    attachments_json: row.get(2)?,
                    content_hash: row.get(3)?,
                    published_date: row.get(4)?,
                    last_seen_at: row.get(5)?,
                    detail_checked_at: row.get(6)?,
                    detail_fetched_at: row.get(7)?,
                    content_changed_at: row.get(8)?,
                })
            },
        )
        .expect("read upsert row")
    }

    #[test]
    fn upsert_detail_fetched_stores_body_and_fetched_at() {
        use rusqlite::params;
        let conn = upsert_test_conn();
        let link = "https://www.aiub.edu/notice/1";
        let now = "2026-11-21T12:00:00Z";
        conn.execute(
            crate::db::upsert_statement_sql(true, true),
            params![
                "Title",
                "Desc",
                link,
                "2026-11-15",
                "Body text",
                "[\"a.pdf\"]",
                "hash1",
                now
            ],
        )
        .expect("insert fetched row");
        let row = read_upsert_row(&conn, link);
        assert_eq!(row.body_text, "Body text");
        assert_eq!(row.attachments_json, "[\"a.pdf\"]");
        assert_eq!(row.detail_fetched_at.as_deref(), Some(now));
        assert_eq!(row.detail_checked_at.as_deref(), Some(now));

        // Conflict update with a new hash: body/hash update, the empty
        // date keeps the stored date (COALESCE), and content_changed_at
        // moves because the hash changed.
        let now2 = "2026-11-21T12:05:00Z";
        conn.execute(
            crate::db::upsert_statement_sql(true, true),
            params!["Title v2", "Desc", link, "", "Body v2", "[]", "hash2", now2],
        )
        .expect("update fetched row");
        let row = read_upsert_row(&conn, link);
        assert_eq!(row.title, "Title v2");
        assert_eq!(row.body_text, "Body v2");
        assert_eq!(row.content_hash, "hash2");
        assert_eq!(
            row.published_date.as_deref(),
            Some("2026-11-15"),
            "an empty date on update must keep the stored date"
        );
        assert_eq!(
            row.content_changed_at, now2,
            "a hash change must move content_changed_at"
        );
    }

    #[test]
    fn upsert_detail_attempted_unfetched_stores_empty_body_and_null_fetched_at() {
        use rusqlite::params;
        let conn = upsert_test_conn();
        let link = "https://www.aiub.edu/notice/2";
        let now = "2026-11-21T12:00:00Z";
        conn.execute(
            crate::db::upsert_statement_sql(true, false),
            params!["Title", "Desc", link, "2026-11-15", "hash1", now],
        )
        .expect("insert attempted row");
        let row = read_upsert_row(&conn, link);
        assert_eq!(row.body_text, "", "unfetched detail stores no body");
        assert_eq!(row.attachments_json, "[]");
        assert_eq!(row.detail_fetched_at, None, "detail_fetched_at stays NULL");
        assert_eq!(row.detail_checked_at.as_deref(), Some(now));

        // Conflict update with the same hash: the title updates but
        // content_changed_at stays — only a hash change moves it.
        let now2 = "2026-11-21T12:05:00Z";
        conn.execute(
            crate::db::upsert_statement_sql(true, false),
            params!["Title v2", "Desc", link, "", "hash1", now2],
        )
        .expect("update attempted row");
        let row = read_upsert_row(&conn, link);
        assert_eq!(row.title, "Title v2");
        assert_eq!(row.detail_fetched_at, None);
        assert_eq!(row.detail_checked_at.as_deref(), Some(now2));
        assert_eq!(
            row.content_changed_at, now,
            "an unchanged hash must not move content_changed_at"
        );
    }

    #[test]
    fn upsert_detail_not_attempted_leaves_detail_timestamps_null() {
        use rusqlite::params;
        let conn = upsert_test_conn();
        let link = "https://www.aiub.edu/notice/3";
        let now = "2026-11-21T12:00:00Z";
        conn.execute(
            crate::db::upsert_statement_sql(false, false),
            params!["Title", "Desc", link, "2026-11-15", "hash1", now],
        )
        .expect("insert unattempted row");
        let row = read_upsert_row(&conn, link);
        assert_eq!(row.body_text, "", "unattempted detail stores no body");
        assert_eq!(row.attachments_json, "[]");
        assert_eq!(row.detail_checked_at, None, "detail_checked_at stays NULL");
        assert_eq!(row.detail_fetched_at, None, "detail_fetched_at stays NULL");
        assert_eq!(row.last_seen_at, now);

        // Conflict update: only the listing columns move; both detail
        // timestamps stay NULL.
        let now2 = "2026-11-21T12:05:00Z";
        conn.execute(
            crate::db::upsert_statement_sql(false, false),
            params!["Title v2", "Desc", link, "", "hash1", now2],
        )
        .expect("update unattempted row");
        let row = read_upsert_row(&conn, link);
        assert_eq!(row.title, "Title v2");
        assert_eq!(row.detail_checked_at, None);
        assert_eq!(row.detail_fetched_at, None);
        assert_eq!(row.last_seen_at, now2);
    }

    // L2: behavioural test — the SQL from build_lightweight_detail_update()
    // (via lightweight_detail_update_sql()) is executed against a real
    // in-memory SQLite database (D1 is SQLite-backed), instead of merely
    // asserting on a source string. This replaces the old string-contains
    // test, which would have passed even if the function were never called.
    #[test]
    fn lightweight_detail_update_preserves_existing_detail_fetched_at() {
        use rusqlite::{Connection, params};

        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        conn.execute_batch(
            "CREATE TABLE notices (
                link TEXT PRIMARY KEY,
                last_seen_at TEXT,
                detail_checked_at TEXT,
                detail_fetched_at TEXT
            )",
        )
        .expect("create notices table");

        // Row 1: detail_fetched_at already set — it must be preserved.
        conn.execute(
            "INSERT INTO notices (link, last_seen_at, detail_checked_at, detail_fetched_at) \
             VALUES (?1, NULL, NULL, ?2)",
            params!["https://www.aiub.edu/notice/1", "2026-11-20T10:00:00Z"],
        )
        .expect("insert row 1");
        // Row 2: detail_fetched_at is NULL — it must be set to `now`.
        conn.execute(
            "INSERT INTO notices (link, last_seen_at, detail_checked_at, detail_fetched_at) \
             VALUES (?1, NULL, NULL, NULL)",
            params!["https://www.aiub.edu/notice/2"],
        )
        .expect("insert row 2");
        // Row 3: detail_fetched_at set, but the update is a non-fetched
        // (detail_fetched=false) one — it must be left untouched.
        conn.execute(
            "INSERT INTO notices (link, last_seen_at, detail_checked_at, detail_fetched_at) \
             VALUES (?1, NULL, NULL, ?2)",
            params!["https://www.aiub.edu/notice/3", "2026-11-19T09:00:00Z"],
        )
        .expect("insert row 3");

        let now = "2026-11-21T12:00:00Z";
        let fetched_sql = crate::db::lightweight_detail_update_sql(true);
        conn.execute(fetched_sql, params![now, "https://www.aiub.edu/notice/1"])
            .expect("run fetched update on row 1");
        conn.execute(fetched_sql, params![now, "https://www.aiub.edu/notice/2"])
            .expect("run fetched update on row 2");

        let unfetched_sql = crate::db::lightweight_detail_update_sql(false);
        conn.execute(unfetched_sql, params![now, "https://www.aiub.edu/notice/3"])
            .expect("run unfetched update on row 3");

        let fetched_at = |link: &str| -> Option<String> {
            conn.query_row(
                "SELECT detail_fetched_at FROM notices WHERE link = ?1",
                params![link],
                |row| row.get(0),
            )
            .expect("read detail_fetched_at")
        };
        let seen_at = |link: &str| -> Option<String> {
            conn.query_row(
                "SELECT last_seen_at FROM notices WHERE link = ?1",
                params![link],
                |row| row.get(0),
            )
            .expect("read last_seen_at")
        };

        assert_eq!(
            fetched_at("https://www.aiub.edu/notice/1").as_deref(),
            Some("2026-11-20T10:00:00Z"),
            "existing detail_fetched_at must be preserved, not overwritten"
        );
        assert_eq!(
            fetched_at("https://www.aiub.edu/notice/2").as_deref(),
            Some(now),
            "NULL detail_fetched_at must be set to now"
        );
        assert_eq!(
            fetched_at("https://www.aiub.edu/notice/3").as_deref(),
            Some("2026-11-19T09:00:00Z"),
            "unfetched update must not touch detail_fetched_at"
        );
        for link in [
            "https://www.aiub.edu/notice/1",
            "https://www.aiub.edu/notice/2",
            "https://www.aiub.edu/notice/3",
        ] {
            assert_eq!(
                seen_at(link).as_deref(),
                Some(now),
                "last_seen_at must be updated for {link}"
            );
        }
    }

    // H1: silent hash sync must never mark a never-sent notice as sent.
    // Row 1: Facebook sent, Telegram never sent — the Telegram sync SQL
    // must leave tg_sent_at NULL and tg_last_notified_hash untouched, while
    // the Facebook sync SQL updates fb_last_notified_hash.
    // Row 2: Telegram sent — the Telegram sync SQL must update the hash
    // and preserve the existing tg_sent_at timestamp.
    #[test]
    fn tg_hash_sync_never_marks_unsent_as_sent() {
        use rusqlite::{Connection, params};

        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        conn.execute_batch(
            "CREATE TABLE notices (
                link TEXT PRIMARY KEY,
                tg_sent_at TEXT,
                tg_last_notified_hash TEXT,
                fb_sent_at TEXT,
                fb_last_notified_hash TEXT
            )",
        )
        .expect("create notices table");
        conn.execute(
            "INSERT INTO notices VALUES (?1, NULL, NULL, ?2, ?3)",
            params![
                "https://www.aiub.edu/notice/1",
                "2026-11-21T10:00:00Z",
                "old_hash"
            ],
        )
        .expect("insert row 1");
        conn.execute(
            "INSERT INTO notices VALUES (?1, ?2, ?3, NULL, NULL)",
            params![
                "https://www.aiub.edu/notice/2",
                "2026-11-21T10:00:00Z",
                "old_hash"
            ],
        )
        .expect("insert row 2");

        let read = |link: &str| -> (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) {
            conn.query_row(
                "SELECT tg_sent_at, tg_last_notified_hash, fb_sent_at, fb_last_notified_hash \
                 FROM notices WHERE link = ?1",
                params![link],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read row")
        };

        conn.execute(
            crate::db::tg_hash_sync_sql(),
            params!["new_hash", "https://www.aiub.edu/notice/1"],
        )
        .expect("tg sync row 1");
        conn.execute(
            crate::db::fb_hash_sync_sql(),
            params!["new_hash", "https://www.aiub.edu/notice/1"],
        )
        .expect("fb sync row 1");
        conn.execute(
            crate::db::tg_hash_sync_sql(),
            params!["new_hash", "https://www.aiub.edu/notice/2"],
        )
        .expect("tg sync row 2");

        let (tg_sent, tg_hash, fb_sent, fb_hash) = read("https://www.aiub.edu/notice/1");
        assert_eq!(tg_sent, None, "row 1: TG sync must not set tg_sent_at");
        assert_eq!(tg_hash, None, "row 1: TG sync must not set tg hash");
        assert_eq!(
            fb_sent.as_deref(),
            Some("2026-11-21T10:00:00Z"),
            "row 1: fb_sent_at must be preserved"
        );
        assert_eq!(
            fb_hash.as_deref(),
            Some("new_hash"),
            "row 1: FB sync must update the hash"
        );

        let (tg_sent, tg_hash, _, _) = read("https://www.aiub.edu/notice/2");
        assert_eq!(
            tg_sent.as_deref(),
            Some("2026-11-21T10:00:00Z"),
            "row 2: tg_sent_at must be preserved"
        );
        assert_eq!(
            tg_hash.as_deref(),
            Some("new_hash"),
            "row 2: TG sync must update the hash for a sent notice"
        );
    }

    // H2: a 506 duplicate means Meta already has this content, so the
    // row must count as sent afterwards — otherwise lib.rs re-queues it
    // as new (fb_sent_at.is_none()) and loops on 506 forever.
    #[test]
    fn fb_duplicate_handled_marks_notice_as_sent() {
        use rusqlite::{Connection, params};

        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        conn.execute_batch(
            "CREATE TABLE notices (
                link TEXT PRIMARY KEY,
                fb_sent_at TEXT,
                fb_last_notified_hash TEXT,
                fb_notification_attempted_at TEXT
            )",
        )
        .expect("create notices table");
        // Row 1: never sent, duplicate reported — must become sent.
        conn.execute(
            "INSERT INTO notices VALUES (?1, NULL, NULL, NULL)",
            params!["https://www.aiub.edu/notice/1"],
        )
        .expect("insert row 1");
        // Row 2: already sent at an earlier time — that timestamp must
        // be preserved, not overwritten.
        conn.execute(
            "INSERT INTO notices VALUES (?1, ?2, ?3, NULL)",
            params![
                "https://www.aiub.edu/notice/2",
                "2026-11-20T09:00:00Z",
                "old_hash"
            ],
        )
        .expect("insert row 2");

        let now = "2026-11-21T12:00:00Z";
        conn.execute(
            crate::db::fb_duplicate_handled_sql(),
            params![now, "hash_v1", now, "https://www.aiub.edu/notice/1"],
        )
        .expect("duplicate-handled row 1");
        conn.execute(
            crate::db::fb_duplicate_handled_sql(),
            params![now, "hash_v2", now, "https://www.aiub.edu/notice/2"],
        )
        .expect("duplicate-handled row 2");

        let read = |link: &str| -> (Option<String>, Option<String>) {
            conn.query_row(
                "SELECT fb_sent_at, fb_last_notified_hash FROM notices WHERE link = ?1",
                params![link],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read row")
        };
        let (sent_at, hash) = read("https://www.aiub.edu/notice/1");
        assert_eq!(
            sent_at.as_deref(),
            Some(now),
            "row 1: duplicate-handled must set fb_sent_at"
        );
        assert_eq!(hash.as_deref(), Some("hash_v1"));
        assert!(
            sent_at.is_some(),
            "row 1: fb_sent_at.is_some() must hold, so it is not re-queued as new"
        );
        let (sent_at, hash) = read("https://www.aiub.edu/notice/2");
        assert_eq!(
            sent_at.as_deref(),
            Some("2026-11-20T09:00:00Z"),
            "row 2: existing fb_sent_at must be preserved"
        );
        assert_eq!(hash.as_deref(), Some("hash_v2"));
    }

    // O7: behavioural test — the SQL from get_app_states()
    // (via app_states_in_sql()) is executed against a real in-memory
    // SQLite database: one IN (...) query must return exactly the
    // requested keys, and no others.
    #[test]
    fn app_states_in_sql_returns_only_requested_keys() {
        use rusqlite::{Connection, params};

        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        conn.execute_batch("CREATE TABLE app_state (key TEXT PRIMARY KEY, value TEXT)")
            .expect("create app_state table");
        for (key, value) in [
            ("fb_auth_paused", "paused:401"),
            ("fb_cooldown_until", "2026-11-23T10:00:00Z"),
            ("unrelated_key", "keep me out"),
        ] {
            conn.execute(
                "INSERT INTO app_state (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .expect("insert app_state row");
        }

        let sql = crate::db::app_states_in_sql(2);
        let mut stmt = conn.prepare(&sql).expect("prepare IN query");
        let rows: Vec<(String, String)> = stmt
            .query_map(params!["fb_auth_paused", "fb_cooldown_until"], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("query app_state")
            .collect::<Result<_, _>>()
            .expect("collect rows");
        assert_eq!(rows.len(), 2, "exactly the two requested keys");
        assert!(
            rows.iter()
                .any(|(k, v)| k == "fb_auth_paused" && v == "paused:401")
        );
        assert!(
            rows.iter()
                .any(|(k, v)| k == "fb_cooldown_until" && v == "2026-11-23T10:00:00Z")
        );
    }

    #[test]
    fn app_states_in_sql_placeholders_match_count() {
        assert_eq!(
            crate::db::app_states_in_sql(1),
            "SELECT key, value FROM app_state WHERE key IN (?1)"
        );
        assert_eq!(
            crate::db::app_states_in_sql(3),
            "SELECT key, value FROM app_state WHERE key IN (?1, ?2, ?3)"
        );
    }
}

#[cfg(test)]
mod utf16_format_tests {
    use crate::models::{Notice, format_notice_message, truncate_text, utf16_len};

    #[test]
    fn utf16_len_counts_emoji_as_two_units() {
        assert_eq!(utf16_len("a"), 1);
        assert_eq!(utf16_len("é"), 1);
        assert_eq!(utf16_len("😀"), 2);
        assert_eq!(utf16_len("a😀b"), 4);
    }

    #[test]
    fn truncate_text_counts_utf16_units_not_chars() {
        // One emoji = 2 UTF-16 units; limit 5 leaves room for one emoji + "...".
        assert_eq!(truncate_text("😀😀😀😀", 5), "😀...");
        assert!(utf16_len(&truncate_text("😀😀😀😀", 5)) <= 5);
        // ASCII behaviour is unchanged.
        assert_eq!(truncate_text("Hello World", 8), "Hello...");
    }

    // M5(a): a body that fits in 4,096 Rust chars but exceeds 4,096 UTF-16
    // units must still be truncated, and the notice URL must survive.
    #[test]
    fn format_notice_message_emoji_heavy_body_respects_utf16_limit() {
        let notice = Notice {
            title: "Emoji Notice".to_string(),
            description: "".to_string(),
            url: "https://www.aiub.edu/notice/emoji".to_string(),
            published_date: Some("2026-11-21".to_string()),
            body_text: "😀".repeat(3000),
            attachments: vec![],
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(
            utf16_len(&msg) <= crate::config::UNIFIED_MAX_MESSAGE_LEN,
            "UTF-16 length {} exceeds 4096 (char count is only {})",
            utf16_len(&msg),
            msg.chars().count()
        );
        assert!(msg.ends_with("https://www.aiub.edu/notice/emoji"));
    }

    // M5(b): image-only notice with a very long attachment list — the
    // notice URL must survive truncation, even if attachments are dropped.
    #[test]
    fn format_notice_message_long_attachment_list_preserves_notice_url() {
        let attachments: Vec<String> = (0..100)
            .map(|i| {
                format!("https://www.aiub.edu/Files/Uploads/flyer-{i:03}-with-a-long-name.webp")
            })
            .collect();
        let notice = Notice {
            title: "Flyer Notice".to_string(),
            description: "No description available".to_string(),
            url: "https://www.aiub.edu/notice/flyer".to_string(),
            published_date: Some("2026-11-21".to_string()),
            body_text: "".to_string(),
            attachments,
            detail_fetched: true,
        };
        let msg = format_notice_message(&notice, false);
        assert!(
            utf16_len(&msg) <= crate::config::UNIFIED_MAX_MESSAGE_LEN,
            "UTF-16 length {} exceeds 4096",
            utf16_len(&msg)
        );
        assert!(
            msg.ends_with("https://www.aiub.edu/notice/flyer"),
            "notice URL must survive truncation, got tail: {:?}",
            msg.chars().rev().take(80).collect::<String>()
        );
        // The message must not contain a truncated copy of the URL with
        // punctuation attached (the full URL itself is asserted above via
        // ends_with).
        assert!(!msg.contains("https://www.aiub.edu/notice/flyer."));
    }
}

#[cfg(test)]
mod auth_pause_tests {
    use crate::facebook::{format_auth_pause_value, parse_auth_pause_value};
    use chrono::{Duration, Utc};

    #[test]
    fn auth_pause_value_round_trips_with_message() {
        let value = format_auth_pause_value("Facebook auth failure (code: 190, subcode: None)");
        let (paused_at, message) = parse_auth_pause_value(&value).expect("must parse");
        assert_eq!(message, "Facebook auth failure (code: 190, subcode: None)");
        // Timestamp is (approximately) now.
        assert!(Utc::now().signed_duration_since(paused_at).num_seconds() < 5);
    }

    #[test]
    fn auth_pause_value_legacy_without_timestamp_does_not_parse() {
        assert!(parse_auth_pause_value("Facebook auth failure (code: 190)").is_none());
        assert!(parse_auth_pause_value("").is_none());
    }

    #[test]
    fn auth_pause_age_exceeds_ttl_after_24h() {
        // H2: a pause older than FACEBOOK_AUTH_PAUSE_TTL_HOURS is expired.
        let old = Utc::now() - Duration::hours(25);
        let value = format!("{}|stale pause", old.to_rfc3339());
        let (paused_at, _) = parse_auth_pause_value(&value).expect("must parse");
        let age_hours = Utc::now().signed_duration_since(paused_at).num_hours();
        assert!(age_hours >= crate::config::FACEBOOK_AUTH_PAUSE_TTL_HOURS);
        let fresh = format_auth_pause_value("fresh pause");
        let (fresh_at, _) = parse_auth_pause_value(&fresh).expect("must parse");
        assert!(
            Utc::now().signed_duration_since(fresh_at).num_hours()
                < crate::config::FACEBOOK_AUTH_PAUSE_TTL_HOURS
        );
    }
}

#[cfg(test)]
mod redact_tests {
    use crate::telegram::redact_token;

    #[test]
    fn redact_token_replaces_exact_token() {
        let msg = "Telegram API HTTP 401: bad token abc123token here";
        assert_eq!(
            redact_token(msg, "abc123token"),
            "Telegram API HTTP 401: bad token [REDACTED] here"
        );
    }

    #[test]
    fn redact_token_replaces_url_encoded_token() {
        // Token contains ':' and '/', which appear percent-encoded in URLs.
        let msg = "GET https://example.com/?access_token=abc%3Adef%2Fghi failed";
        assert!(!redact_token(msg, "abc:def/ghi").contains("abc%3Adef%2Fghi"));
        assert!(redact_token(msg, "abc:def/ghi").contains("[REDACTED]"));
    }

    #[test]
    fn redact_token_replaces_eaa_pattern_and_truncated_fragment() {
        let msg = "Meta HTTP 401: token EAA123abcXYZ789 leaked";
        assert!(!redact_token(msg, "some-other-token").contains("EAA123abcXYZ789"));
        // A long token's first 20 chars appearing alone must also be redacted.
        let long_token = "abcdefghijklmnopqrstuvwxyz0123456789";
        let fragment_msg = format!("error with {} in it", &long_token[..20]);
        assert!(!redact_token(&fragment_msg, long_token).contains(&long_token[..20]));
    }

    #[test]
    fn redact_token_leaves_benign_text_alone() {
        assert_eq!(
            redact_token("plain error, no token", "abc123token"),
            "plain error, no token"
        );
        // A bare "EAA" with no token body after it is not a token.
        assert_eq!(redact_token("EAA", "abc123token"), "EAA");
    }
}

#[cfg(test)]
mod error_tests {
    use crate::errors::AppError;

    #[test]
    fn app_error_display_labels_the_layer() {
        assert_eq!(
            AppError::Fetch("boom".to_string()).to_string(),
            "fetch error: boom"
        );
        assert_eq!(
            AppError::D1("boom".to_string()).to_string(),
            "D1 error: boom"
        );
        assert_eq!(
            AppError::Config("boom".to_string()).to_string(),
            "config error: boom"
        );
    }

    #[test]
    fn app_error_converts_to_worker_error() {
        let err: worker::Error =
            AppError::Config("missing TELEGRAM_BOT_API_KEY secret: test".to_string()).into();
        assert!(err.to_string().contains("config error:"));
    }
}

#[cfg(test)]
mod dispatch_tests {
    use crate::config::{
        FACEBOOK_BUC_MAX_COOLDOWN_MINS, FACEBOOK_COOLDOWN_UNTIL_KEY, FacebookSecrets, Secrets,
        TELEGRAM_AUTH_PAUSED_KEY,
    };
    use crate::dispatch::{
        DispatchLog, DispatchOutcome, FacebookSendStep, FacebookSender, MarkSink,
        PendingNotification, SideEffectSink, TelegramSendStep, TelegramSender,
        dispatch_facebook_phase, dispatch_telegram_phase, facebook_send_step,
        flush_dispatch_side_effects, fold_mark_batch_result, telegram_send_step,
    };
    use crate::errors::AppError;
    use crate::facebook::{
        BucUsage, FacebookPostResult, classify_publish_error, parse_auth_pause_value,
        parse_buc_usage_header,
    };
    use crate::models::{D1Metrics, Notice};
    use crate::telegram::TelegramSendResult;
    use chrono::{DateTime, Utc};
    use futures_util::future::FutureExt;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    // ── Fakes ────────────────────────────────────────────────────────
    //
    // The dispatch traits exist precisely so the phases can run
    // natively: the real sender needs Worker `Fetch`, real mark
    // building needs a `D1Database`, and the `console_*` macros abort
    // on non-wasm targets. These fakes record every call in order.

    struct FakeTelegramSender {
        script: RefCell<VecDeque<worker::Result<TelegramSendResult>>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeTelegramSender {
        fn new(script: Vec<worker::Result<TelegramSendResult>>) -> Self {
            Self {
                script: RefCell::new(script.into()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl TelegramSender for FakeTelegramSender {
        async fn send_notice(
            &self,
            _secrets: &Secrets,
            message: &str,
        ) -> worker::Result<TelegramSendResult> {
            self.calls.borrow_mut().push(message.to_string());
            self.script
                .borrow_mut()
                .pop_front()
                .expect("fake Telegram sender ran out of scripted results")
        }
    }

    struct FakeFacebookSender {
        script: RefCell<VecDeque<worker::Result<FacebookPostResult>>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeFacebookSender {
        fn new(script: Vec<worker::Result<FacebookPostResult>>) -> Self {
            Self {
                script: RefCell::new(script.into()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl FacebookSender for FakeFacebookSender {
        async fn publish(
            &self,
            _secrets: &FacebookSecrets,
            message: &str,
        ) -> worker::Result<FacebookPostResult> {
            self.calls.borrow_mut().push(message.to_string());
            self.script
                .borrow_mut()
                .pop_front()
                .expect("fake Facebook sender ran out of scripted results")
        }
    }

    struct FakeMarkSink {
        calls: Vec<String>,
        fail_on: Vec<&'static str>,
    }

    impl FakeMarkSink {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                fail_on: Vec::new(),
            }
        }

        fn failing(fail_on: Vec<&'static str>) -> Self {
            Self {
                calls: Vec::new(),
                fail_on,
            }
        }

        fn record(&mut self, key: String) -> worker::Result<()> {
            if self.fail_on.iter().any(|prefix| key.starts_with(prefix)) {
                return Err(AppError::D1(format!("fake mark failure for {key}")).into());
            }
            self.calls.push(key);
            Ok(())
        }
    }

    impl MarkSink for FakeMarkSink {
        fn record_tg_attempt(
            &mut self,
            _outcome: &mut DispatchOutcome,
            url: &str,
            _now: &str,
        ) -> worker::Result<()> {
            self.record(format!("tg_attempt:{url}"))
        }

        fn record_tg_notified(
            &mut self,
            _outcome: &mut DispatchOutcome,
            url: &str,
            _content_hash: &str,
            _now: &str,
        ) -> worker::Result<()> {
            self.record(format!("tg_notified:{url}"))
        }

        fn record_fb_attempt(
            &mut self,
            _outcome: &mut DispatchOutcome,
            url: &str,
            _now: &str,
        ) -> worker::Result<()> {
            self.record(format!("fb_attempt:{url}"))
        }

        fn record_fb_notified(
            &mut self,
            _outcome: &mut DispatchOutcome,
            url: &str,
            _content_hash: &str,
            post_id: &str,
            _now: &str,
        ) -> worker::Result<()> {
            self.record(format!("fb_notified:{url}:{post_id}"))
        }

        fn record_fb_duplicate(
            &mut self,
            _outcome: &mut DispatchOutcome,
            url: &str,
            _content_hash: &str,
            _now: &str,
        ) -> worker::Result<()> {
            self.record(format!("fb_duplicate:{url}"))
        }
    }

    struct FakeLog {
        lines: Vec<(bool, String)>,
    }

    impl FakeLog {
        fn new() -> Self {
            Self { lines: Vec::new() }
        }

        fn warns(&self) -> Vec<&str> {
            self.lines
                .iter()
                .filter(|(is_warn, _)| *is_warn)
                .map(|(_, m)| m.as_str())
                .collect()
        }

        fn errors(&self) -> Vec<&str> {
            self.lines
                .iter()
                .filter(|(is_warn, _)| !*is_warn)
                .map(|(_, m)| m.as_str())
                .collect()
        }
    }

    impl DispatchLog for FakeLog {
        fn warn(&mut self, message: String) {
            self.lines.push((true, message));
        }

        fn error(&mut self, message: String) {
            self.lines.push((false, message));
        }
    }

    // ── Fake side-effect sink (audit #15) ────────────────────────────
    //
    // Records every side-effect call in order; `failing` makes the
    // named kinds fail so the flush's non-fatal handling is testable.

    struct FakeSideEffectSink {
        calls: RefCell<Vec<String>>,
        fail_on: Vec<&'static str>,
    }

    impl FakeSideEffectSink {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_on: Vec::new(),
            }
        }

        fn failing(fail_on: Vec<&'static str>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_on,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn record(&self, key: String) -> worker::Result<()> {
            // Record the attempt even when it fails: the flush test
            // asserts that later side effects still run after a
            // failure, so failed attempts must be visible too.
            self.calls.borrow_mut().push(key.clone());
            if self.fail_on.iter().any(|kind| key.starts_with(kind)) {
                return Err(AppError::D1(format!("fake side-effect failure for {key}")).into());
            }
            Ok(())
        }
    }

    impl SideEffectSink for FakeSideEffectSink {
        async fn set_app_state(&self, key: &str, value: &str) -> worker::Result<D1Metrics> {
            self.record(format!("set_app_state:{key}={value}"))?;
            Ok(D1Metrics::default())
        }

        async fn send_fb_alert(&self, category: &str, _message: &str) -> worker::Result<()> {
            self.record(format!("fb_alert:{category}"))
        }

        async fn send_fb_auth_alert(&self, _message: &str) -> worker::Result<()> {
            self.record("fb_auth_alert".to_string())
        }

        async fn send_tg_auth_alert(&self, _message: &str) -> worker::Result<()> {
            self.record("tg_auth_alert".to_string())
        }
    }

    fn test_secrets() -> Secrets {
        Secrets {
            telegram_bot_api_key: "test-bot-key".to_string(),
            telegram_chat_id: "123".to_string(),
            telegram_admin_chat_id: None,
            facebook: None,
        }
    }

    fn test_fb_secrets() -> FacebookSecrets {
        FacebookSecrets::from_raw(Some("1234567890"), Some("EAABtest-token"))
            .expect("valid fb secrets")
    }

    fn notification(url: &str) -> PendingNotification {
        PendingNotification {
            notice: Notice {
                title: format!("Notice {url}"),
                description: "Desc".to_string(),
                url: url.to_string(),
                published_date: None,
                body_text: "Body".to_string(),
                attachments: vec![],
                detail_fetched: true,
            },
            content_hash: format!("hash-{url}"),
            message: format!("Message for {url}"),
        }
    }

    fn minutes_until(rfc3339: &str) -> i64 {
        let dt = DateTime::parse_from_rfc3339(rfc3339)
            .expect("cooldown value is RFC3339")
            .with_timezone(&Utc);
        dt.signed_duration_since(Utc::now()).num_minutes()
    }

    fn run_tg_phase(
        queue: Vec<PendingNotification>,
        paused: bool,
        sender: &FakeTelegramSender,
        marks: &mut FakeMarkSink,
        log: &mut FakeLog,
    ) -> DispatchOutcome {
        let secrets = test_secrets();
        dispatch_telegram_phase(&secrets, queue, paused, sender, marks, log)
            .now_or_never()
            .expect("Telegram phase completes synchronously with a fake sender")
    }

    fn run_fb_phase(
        queue: Vec<PendingNotification>,
        paused: bool,
        sender: &FakeFacebookSender,
        marks: &mut FakeMarkSink,
        log: &mut FakeLog,
    ) -> DispatchOutcome {
        let secrets = test_fb_secrets();
        dispatch_facebook_phase(&secrets, queue, paused, sender, marks, log)
            .now_or_never()
            .expect("Facebook phase completes synchronously with a fake sender")
    }

    // ── (a) Telegram auth pause ──────────────────────────────────────

    #[test]
    fn tg_phase_auth_failure_pauses_and_stops() {
        // 401/403 sets tg_auth_paused and stops the phase: the third
        // notice is never attempted.
        let sender = FakeTelegramSender::new(vec![
            Ok(TelegramSendResult::Sent),
            Ok(TelegramSendResult::AuthFailure {
                message: "Telegram API HTTP 403: Forbidden: bot was blocked by the user"
                    .to_string(),
            }),
            Ok(TelegramSendResult::Sent),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
            notification("https://www.aiub.edu/notice/3"),
        ];
        let outcome = run_tg_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(outcome.failed_count, 1);
        assert_eq!(outcome.deferred_count, 0);
        assert_eq!(
            sender.calls().len(),
            2,
            "phase must stop after the auth failure and never send the third notice"
        );
        assert_eq!(
            marks.calls,
            vec![
                "tg_attempt:https://www.aiub.edu/notice/1",
                "tg_notified:https://www.aiub.edu/notice/1",
                "tg_attempt:https://www.aiub.edu/notice/2",
            ]
        );
        // The pause is collected for the post-join flush: timestamped
        // and parseable, carrying the auth message.
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, TELEGRAM_AUTH_PAUSED_KEY);
        let (_, message) =
            parse_auth_pause_value(&outcome.app_state_sets[0].1).expect("pause value parses");
        assert!(message.contains("403"));
        assert!(outcome.tg_auth_failure.as_deref().unwrap().contains("403"));
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Telegram Auth Error]"))
        );
    }

    #[test]
    fn tg_send_step_auth_failure_collects_pause_and_alert() {
        let mut outcome = DispatchOutcome::default();
        let result: worker::Result<TelegramSendResult> = Ok(TelegramSendResult::AuthFailure {
            message: "Telegram API HTTP 401: Unauthorized".to_string(),
        });
        let step = telegram_send_step(&mut outcome, "https://www.aiub.edu/notice/1", &result);
        assert!(matches!(step, TelegramSendStep::AuthFailureStop { .. }));
        assert_eq!(outcome.failed_count, 1);
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, TELEGRAM_AUTH_PAUSED_KEY);
        let (paused_at, message) =
            parse_auth_pause_value(&outcome.app_state_sets[0].1).expect("pause value parses");
        assert_eq!(message, "Telegram API HTTP 401: Unauthorized");
        assert!(
            Utc::now().signed_duration_since(paused_at).num_seconds() < 5,
            "pause timestamp is approximately now"
        );
        assert_eq!(
            outcome.tg_auth_failure.as_deref(),
            Some(
                "🚨 AIUB Notice Telegram: Authentication failed: Telegram API HTTP 401: Unauthorized"
            )
        );
    }

    #[test]
    fn tg_phase_deferred_stops_without_pause() {
        // A 429 beyond the inline budget surfaces as Deferred: the
        // phase defers the rest of the queue without setting any pause.
        let sender = FakeTelegramSender::new(vec![
            Ok(TelegramSendResult::Sent),
            Ok(TelegramSendResult::Deferred),
            Ok(TelegramSendResult::Sent),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
            notification("https://www.aiub.edu/notice/3"),
        ];
        let outcome = run_tg_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(outcome.deferred_count, 1);
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(sender.calls().len(), 2);
        assert!(
            outcome.app_state_sets.is_empty(),
            "a deferral sets no pause"
        );
        assert!(outcome.tg_auth_failure.is_none());
        assert_eq!(
            marks.calls,
            vec![
                "tg_attempt:https://www.aiub.edu/notice/1",
                "tg_notified:https://www.aiub.edu/notice/1",
                "tg_attempt:https://www.aiub.edu/notice/2",
            ]
        );
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Telegram Deferred]"))
        );
    }

    #[test]
    fn tg_send_step_deferred_counts_and_stops() {
        let mut outcome = DispatchOutcome::default();
        let result: worker::Result<TelegramSendResult> = Ok(TelegramSendResult::Deferred);
        let step = telegram_send_step(&mut outcome, "https://www.aiub.edu/notice/1", &result);
        assert!(matches!(step, TelegramSendStep::DeferredStop { .. }));
        assert_eq!(outcome.deferred_count, 1);
        assert!(outcome.app_state_sets.is_empty());
    }

    #[test]
    fn tg_phase_send_error_continues_to_next_notice() {
        // A generic send error is not an auth failure and not a rate
        // limit: it counts as failed and the phase continues.
        let sender = FakeTelegramSender::new(vec![
            Err(worker::Error::RustError("boom".into())),
            Ok(TelegramSendResult::Sent),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_tg_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(outcome.failed_count, 1);
        assert_eq!(sender.calls().len(), 2);
        assert_eq!(
            marks.calls,
            vec![
                "tg_attempt:https://www.aiub.edu/notice/1",
                "tg_attempt:https://www.aiub.edu/notice/2",
                "tg_notified:https://www.aiub.edu/notice/2",
            ]
        );
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Telegram Send Failed]"))
        );
    }

    #[test]
    fn tg_phase_skipped_while_paused() {
        // A pause set by a previous run's 401/403 skips Telegram
        // dispatch before any send is attempted.
        let sender = FakeTelegramSender::new(vec![Ok(TelegramSendResult::Sent)]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let outcome = run_tg_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            true,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(sender.calls().len(), 0, "no send while paused");
        assert!(marks.calls.is_empty());
        assert_eq!(outcome.sent_count, 0);
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(outcome.deferred_count, 0);
    }

    #[test]
    fn tg_phase_empty_queue_returns_default_outcome() {
        let sender = FakeTelegramSender::new(vec![Ok(TelegramSendResult::Sent)]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let outcome = run_tg_phase(vec![], false, &sender, &mut marks, &mut log);
        assert_eq!(sender.calls().len(), 0);
        assert!(marks.calls.is_empty());
        assert_eq!(outcome.sent_count, 0);
    }

    #[test]
    fn tg_phase_notified_mark_failure_counts_failed_not_sent() {
        // M4: a send that cannot be marked counts as failed (it may
        // re-send next run) instead of being counted as sent.
        let sender = FakeTelegramSender::new(vec![
            Ok(TelegramSendResult::Sent),
            Ok(TelegramSendResult::Sent),
        ]);
        let mut marks = FakeMarkSink::failing(vec!["tg_notified"]);
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_tg_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 0);
        assert_eq!(outcome.failed_count, 2);
        assert_eq!(
            marks.calls,
            vec![
                "tg_attempt:https://www.aiub.edu/notice/1",
                "tg_attempt:https://www.aiub.edu/notice/2",
            ]
        );
        assert_eq!(log.errors().len(), 2);
        assert!(
            log.errors()
                .iter()
                .all(|m| m.contains("[Telegram Mark Failed]"))
        );
    }

    #[test]
    fn tg_phase_attempt_mark_failure_still_sends() {
        // A failed attempt mark must not block the send itself.
        let sender = FakeTelegramSender::new(vec![Ok(TelegramSendResult::Sent)]);
        let mut marks = FakeMarkSink::failing(vec!["tg_attempt"]);
        let mut log = FakeLog::new();
        let outcome = run_tg_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            false,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(sender.calls().len(), 1);
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Telegram Attempt Mark Failed]"))
        );
    }

    // ── (a) 401/403 classification + bounded 429 retry ───────────────

    #[test]
    fn telegram_status_classifies_401_and_403_as_auth_failure() {
        use crate::telegram::{TelegramStatusAction, classify_telegram_status};
        assert_eq!(classify_telegram_status(200), TelegramStatusAction::Success);
        assert_eq!(classify_telegram_status(201), TelegramStatusAction::Success);
        assert_eq!(
            classify_telegram_status(429),
            TelegramStatusAction::RateLimited
        );
        assert_eq!(
            classify_telegram_status(401),
            TelegramStatusAction::AuthFailure
        );
        assert_eq!(
            classify_telegram_status(403),
            TelegramStatusAction::AuthFailure
        );
        assert_eq!(classify_telegram_status(500), TelegramStatusAction::Error);
        assert_eq!(classify_telegram_status(400), TelegramStatusAction::Error);
    }

    #[test]
    fn telegram_429_retry_is_bounded() {
        use crate::telegram::should_retry_telegram_429;
        // Attempts remain and retry_after fits the 10s inline budget.
        assert!(should_retry_telegram_429(0, 5));
        assert!(should_retry_telegram_429(1, 10));
        // Out of attempts (TELEGRAM_MAX_RETRIES = 2): defer, never retry again.
        assert!(!should_retry_telegram_429(2, 5));
        assert!(!should_retry_telegram_429(3, 1));
        // retry_after exceeds the inline budget: defer even on the first attempt.
        assert!(!should_retry_telegram_429(0, 11));
        assert!(!should_retry_telegram_429(0, 60));
    }

    // ── Audit #3: a 2xx with ok == false is not a delivery ───────────

    #[test]
    fn telegram_body_delivered_requires_ok_true() {
        use crate::telegram::telegram_body_delivered;
        assert!(telegram_body_delivered(r#"{"ok":true,"result":{}}"#));
        assert!(
            !telegram_body_delivered(r#"{"ok":false,"description":"Forbidden: bot was blocked"}"#),
            "ok:false must not count as delivered"
        );
        assert!(!telegram_body_delivered("{}"), "missing ok");
        assert!(!telegram_body_delivered("not json"), "unparsable body");
    }

    // ── Facebook phase ───────────────────────────────────────────────

    #[test]
    fn fb_phase_success_records_marks_in_order() {
        let sender = FakeFacebookSender::new(vec![
            Ok(FacebookPostResult::Success {
                post_id: "1_1".to_string(),
                usage: None,
            }),
            Ok(FacebookPostResult::Success {
                post_id: "1_2".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 2);
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(outcome.deferred_count, 0);
        assert_eq!(sender.calls().len(), 2);
        assert_eq!(
            marks.calls,
            vec![
                "fb_attempt:https://www.aiub.edu/notice/1",
                "fb_notified:https://www.aiub.edu/notice/1:1_1",
                "fb_attempt:https://www.aiub.edu/notice/2",
                "fb_notified:https://www.aiub.edu/notice/2:1_2",
            ]
        );
        assert!(log.lines.is_empty(), "clean success logs nothing");
    }

    #[test]
    fn fb_phase_duplicate_then_rate_limited_collects_writes_in_order() {
        // (d) FB write-flush ordering: the phase only *collects* D1
        // writes (marks, app_state sets, alerts) while running — the
        // flush happens after the join in dispatch_both_phases. The
        // collected writes must be in causal order: the duplicate mark
        // and alert for notice 1, then the attempt mark, cooldown set
        // and alert for notice 2; notice 3 is never attempted.
        let sender = FakeFacebookSender::new(vec![
            Ok(FacebookPostResult::Duplicate),
            Ok(FacebookPostResult::RateLimited {
                cooldown_minutes: 60,
            }),
            Ok(FacebookPostResult::Success {
                post_id: "1_3".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
            notification("https://www.aiub.edu/notice/3"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 0);
        // Audit #13: the duplicate must be counted, not invisible —
        // a run of all duplicates used to look like an empty queue.
        assert_eq!(outcome.duplicate_count, 1);
        assert_eq!(outcome.deferred_count, 1);
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(sender.calls().len(), 2, "rate limit stops the phase");
        assert_eq!(
            marks.calls,
            vec![
                "fb_attempt:https://www.aiub.edu/notice/1",
                "fb_duplicate:https://www.aiub.edu/notice/1",
                "fb_attempt:https://www.aiub.edu/notice/2",
            ]
        );
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["duplicate_post", "rate_limit"],
            "alerts are collected in the order the errors happened"
        );
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(
            outcome.app_state_sets[0].0, FACEBOOK_COOLDOWN_UNTIL_KEY,
            "only the rate-limit cooldown is collected"
        );
        assert!(
            (59..=60).contains(&minutes_until(&outcome.app_state_sets[0].1)),
            "cooldown is ~60 minutes out"
        );
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Facebook Duplicate]"))
        );
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Facebook Rate Limited]"))
        );
    }

    #[test]
    fn fb_send_step_rate_limited_collects_cooldown_and_alert() {
        let mut outcome = DispatchOutcome::default();
        let result: worker::Result<FacebookPostResult> = Ok(FacebookPostResult::RateLimited {
            cooldown_minutes: 60,
        });
        let step = facebook_send_step(&mut outcome, "https://www.aiub.edu/notice/1", &result);
        assert!(matches!(step, FacebookSendStep::RateLimitedStop { .. }));
        assert_eq!(outcome.deferred_count, 1);
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, FACEBOOK_COOLDOWN_UNTIL_KEY);
        assert!(
            (59..=60).contains(&minutes_until(&outcome.app_state_sets[0].1)),
            "cooldown is ~60 minutes out"
        );
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["rate_limit"]
        );
    }

    #[test]
    fn fb_send_step_duplicate_collects_alert_and_continues() {
        let mut outcome = DispatchOutcome::default();
        let result: worker::Result<FacebookPostResult> = Ok(FacebookPostResult::Duplicate);
        let step = facebook_send_step(&mut outcome, "https://www.aiub.edu/notice/1", &result);
        assert!(matches!(step, FacebookSendStep::RecordDuplicate));
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(outcome.deferred_count, 0);
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["duplicate_post"]
        );
        assert!(outcome.app_state_sets.is_empty());
    }

    #[test]
    fn fb_phase_buc_high_sets_cooldown_and_stops() {
        // BUC usage approaching the limit: the post still counts as
        // sent, the 15-minute cooldown is collected, and the phase
        // stops after this item.
        let sender = FakeFacebookSender::new(vec![
            Ok(FacebookPostResult::Success {
                post_id: "1_1".to_string(),
                usage: Some(BucUsage {
                    call_count: 85,
                    total_cputime: 10,
                    total_time: 10,
                    estimated_time_to_regain_access: 0,
                }),
            }),
            Ok(FacebookPostResult::Success {
                post_id: "1_2".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(sender.calls().len(), 1, "BUC-high stops the phase");
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, FACEBOOK_COOLDOWN_UNTIL_KEY);
        assert!(
            (14..=15).contains(&minutes_until(&outcome.app_state_sets[0].1)),
            "BUC cooldown is ~15 minutes out"
        );
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Facebook BUC High]") && m.contains("85%"))
        );
    }

    // ── Audit #14: the success cooldown honours the regain estimate ──

    #[test]
    fn fb_phase_buc_high_cooldown_uses_regain_estimate() {
        // Usage at 95% with a 120-minute regain estimate: the cooldown
        // must be ~120 minutes, not the 15-minute floor — otherwise
        // the next run resumes while still throttled.
        let sender = FakeFacebookSender::new(vec![Ok(FacebookPostResult::Success {
            post_id: "1_1".to_string(),
            usage: Some(BucUsage {
                call_count: 95,
                total_cputime: 10,
                total_time: 10,
                estimated_time_to_regain_access: 120,
            }),
        })]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![notification("https://www.aiub.edu/notice/1")];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, FACEBOOK_COOLDOWN_UNTIL_KEY);
        assert!(
            (119..=120).contains(&minutes_until(&outcome.app_state_sets[0].1)),
            "BUC cooldown is ~120 minutes out (the regain estimate)"
        );
    }

    #[test]
    fn fb_send_step_success_with_buc_high_collects_cooldown() {
        let mut outcome = DispatchOutcome::default();
        let result: worker::Result<FacebookPostResult> = Ok(FacebookPostResult::Success {
            post_id: "1_1".to_string(),
            usage: Some(BucUsage {
                call_count: 82,
                total_cputime: 50,
                total_time: 40,
                estimated_time_to_regain_access: 15,
            }),
        });
        let step = facebook_send_step(&mut outcome, "https://www.aiub.edu/notice/1", &result);
        match step {
            FacebookSendStep::RecordNotified {
                post_id,
                cooldown_until,
            } => {
                assert_eq!(post_id, "1_1");
                let (until, max_usage) = cooldown_until.expect("BUC-high collects a cooldown");
                assert_eq!(max_usage, 82);
                assert!((14..=15).contains(&minutes_until(&until)));
            }
            other => panic!("expected RecordNotified with cooldown, got {other:?}"),
        }
        assert_eq!(outcome.app_state_sets.len(), 1);
    }

    #[test]
    fn fb_phase_auth_error_pauses_and_alerts() {
        let sender = FakeFacebookSender::new(vec![
            Ok(FacebookPostResult::AuthError {
                message: "Facebook auth failure (code: 190, subcode: Some(458)): Invalid OAuth access token"
                    .to_string(),
            }),
            Ok(FacebookPostResult::Success {
                post_id: "1_2".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.failed_count, 1);
        assert_eq!(outcome.sent_count, 0);
        assert_eq!(sender.calls().len(), 1, "auth failure stops the phase");
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(
            outcome.app_state_sets[0].0,
            crate::config::FACEBOOK_AUTH_PAUSED_KEY
        );
        let (_, message) =
            parse_auth_pause_value(&outcome.app_state_sets[0].1).expect("pause value parses");
        assert!(message.contains("190"));
        assert!(outcome.fb_auth_alert.as_deref().unwrap().contains("190"));
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Facebook Auth Error]"))
        );
    }

    #[test]
    fn fb_phase_spam_block_sets_twelve_hour_cooldown() {
        let sender = FakeFacebookSender::new(vec![Ok(FacebookPostResult::SpamBlocked)]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let outcome = run_fb_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            false,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(outcome.failed_count, 1);
        assert_eq!(outcome.deferred_count, 0);
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["spam_block"]
        );
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, FACEBOOK_COOLDOWN_UNTIL_KEY);
        assert!(
            (719..=720).contains(&minutes_until(&outcome.app_state_sets[0].1)),
            "spam-block cooldown is ~12 hours out"
        );
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Facebook Spam Block]"))
        );
    }

    #[test]
    fn fb_phase_transient_defers_without_cooldown() {
        let sender = FakeFacebookSender::new(vec![Ok(FacebookPostResult::TransientFailure {
            message: "Meta HTTP 500: internal error".to_string(),
        })]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let outcome = run_fb_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            false,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(outcome.deferred_count, 1);
        assert_eq!(outcome.failed_count, 0);
        assert!(
            outcome.app_state_sets.is_empty(),
            "no cooldown for transient"
        );
        assert!(outcome.fb_alerts.is_empty(), "no alert for transient");
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Facebook Transient]"))
        );
    }

    #[test]
    fn fb_phase_unknown_error_alerts_without_cooldown_or_pause() {
        // M10: an unclassified Meta error alerts once (debounced) but
        // sets no cooldown and no pause.
        let sender = FakeFacebookSender::new(vec![Ok(FacebookPostResult::UnknownError {
            message: "Meta error 99999 (subcode: None): something new".to_string(),
        })]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let outcome = run_fb_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            false,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(outcome.failed_count, 1);
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["unknown_error"]
        );
        assert!(
            outcome.app_state_sets.is_empty(),
            "no cooldown or pause for an unclassified error"
        );
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Facebook Unknown Error]"))
        );
    }

    #[test]
    fn fb_phase_worker_error_continues() {
        // Audit #4: a worker error counts as failed but must not abort
        // the rest of the queue — only rate-limit, spam, auth and
        // BUC-high stop the phase.
        let sender = FakeFacebookSender::new(vec![
            Err(worker::Error::RustError("fetch error: boom".into())),
            Ok(FacebookPostResult::Success {
                post_id: "1_2".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.failed_count, 1);
        assert_eq!(outcome.sent_count, 1);
        assert_eq!(
            sender.calls().len(),
            2,
            "worker error continues to the next notice"
        );
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Facebook Worker Error]"))
        );
    }

    #[test]
    fn fb_phase_transient_and_unknown_continue() {
        // Audit #4: a transient failure defers its own notice and an
        // unclassified error fails its own notice, but neither stops
        // the phase — the notice behind them still posts.
        let sender = FakeFacebookSender::new(vec![
            Ok(FacebookPostResult::TransientFailure {
                message: "Meta HTTP 500: internal error".to_string(),
            }),
            Ok(FacebookPostResult::UnknownError {
                message: "Meta error 99999 (subcode: None): something new".to_string(),
            }),
            Ok(FacebookPostResult::Success {
                post_id: "1_3".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
            notification("https://www.aiub.edu/notice/3"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.deferred_count, 1);
        assert_eq!(outcome.failed_count, 1);
        assert_eq!(outcome.sent_count, 1);
        assert_eq!(sender.calls().len(), 3, "all three notices attempted");
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["unknown_error"],
            "the unknown error still collects its debounced alert"
        );
    }

    // ── Audit #15: side-effect flush order and non-fatal failure ────

    fn flush_outcome() -> DispatchOutcome {
        DispatchOutcome {
            app_state_sets: vec![
                (
                    "fb_cooldown_until".to_string(),
                    "2026-11-24T00:00:00Z".to_string(),
                ),
                ("fb_auth_paused".to_string(), "paused".to_string()),
            ],
            fb_alerts: vec![
                ("rate_limit".to_string(), "rate limited".to_string()),
                ("unknown_error".to_string(), "unknown".to_string()),
            ],
            fb_auth_alert: Some("fb auth failed".to_string()),
            tg_auth_failure: Some("tg auth failed".to_string()),
            ..Default::default()
        }
    }

    fn run_side_effect_flush(
        sink: &FakeSideEffectSink,
        log: &mut FakeLog,
        outcome: DispatchOutcome,
    ) {
        flush_dispatch_side_effects(sink, log, "Facebook", outcome)
            .now_or_never()
            .expect("side-effect flush completes synchronously with a fake sink");
    }

    #[test]
    fn flush_side_effects_runs_in_order() {
        let sink = FakeSideEffectSink::new();
        let mut log = FakeLog::new();
        run_side_effect_flush(&sink, &mut log, flush_outcome());

        // Causal order: app_state sets, then debounced alerts, then
        // the auth alerts — a swapped order here is a real bug the
        // old code could only catch on a live D1 run.
        assert_eq!(
            sink.calls(),
            vec![
                "set_app_state:fb_cooldown_until=2026-11-24T00:00:00Z",
                "set_app_state:fb_auth_paused=paused",
                "fb_alert:rate_limit",
                "fb_alert:unknown_error",
                "fb_auth_alert",
                "tg_auth_alert",
            ]
        );
        assert!(log.warns().is_empty(), "a clean flush logs nothing");
    }

    #[test]
    fn flush_side_effects_failure_is_non_fatal() {
        let sink = FakeSideEffectSink::failing(vec!["set_app_state", "fb_alert"]);
        let mut log = FakeLog::new();
        run_side_effect_flush(&sink, &mut log, flush_outcome());

        // Every side effect is still attempted, in order — a failed
        // app_state set or alert must not abort the rest of the flush.
        assert_eq!(
            sink.calls(),
            vec![
                "set_app_state:fb_cooldown_until=2026-11-24T00:00:00Z",
                "set_app_state:fb_auth_paused=paused",
                "fb_alert:rate_limit",
                "fb_alert:unknown_error",
                "fb_auth_alert",
                "tg_auth_alert",
            ]
        );
        assert_eq!(
            log.warns().len(),
            4,
            "two failed sets + two failed alerts warn, the auth alerts succeed silently"
        );
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Dispatch State Flush Failed]"))
        );
        assert!(
            log.warns()
                .iter()
                .any(|m| m.contains("[Dispatch Alert Flush Failed]"))
        );
    }

    // ── Audit #15: mark-flush failure counting ───────────────────────

    #[test]
    fn fold_mark_batch_result_counts_failures() {
        let mut metrics = D1Metrics::default();
        fold_mark_batch_result(
            &mut metrics,
            &Ok(D1Metrics {
                rows_read: 2,
                rows_written: 3,
                failed_statements: 1,
            }),
            5,
        );
        assert_eq!(metrics.rows_read, 2);
        assert_eq!(metrics.rows_written, 3);
        assert_eq!(metrics.failed_statements, 1);
        fold_mark_batch_result(
            &mut metrics,
            &Err(worker::Error::RustError("boom".into())),
            4,
        );
        assert_eq!(
            metrics.failed_statements, 5,
            "a failed batch contributes its whole statement count"
        );
    }

    #[test]
    fn fb_phase_skipped_while_paused() {
        let sender = FakeFacebookSender::new(vec![Ok(FacebookPostResult::Success {
            post_id: "1_1".to_string(),
            usage: None,
        })]);
        let mut marks = FakeMarkSink::new();
        let mut log = FakeLog::new();
        let outcome = run_fb_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            true,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(sender.calls().len(), 0, "no publish while paused");
        assert!(marks.calls.is_empty());
        assert_eq!(outcome.sent_count, 0);
    }

    #[test]
    fn fb_phase_notified_mark_failure_counts_failed() {
        let sender = FakeFacebookSender::new(vec![Ok(FacebookPostResult::Success {
            post_id: "1_1".to_string(),
            usage: None,
        })]);
        let mut marks = FakeMarkSink::failing(vec!["fb_notified"]);
        let mut log = FakeLog::new();
        let outcome = run_fb_phase(
            vec![notification("https://www.aiub.edu/notice/1")],
            false,
            &sender,
            &mut marks,
            &mut log,
        );

        assert_eq!(outcome.sent_count, 0);
        assert_eq!(outcome.failed_count, 1);
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Facebook Mark Failed]"))
        );
    }

    #[test]
    fn fb_phase_duplicate_mark_failure_counts_failed_and_continues() {
        // M4: a failed duplicate mark counts as failed but must not
        // abort the remaining dispatch; the debounced alert is still
        // collected.
        let sender = FakeFacebookSender::new(vec![
            Ok(FacebookPostResult::Duplicate),
            Ok(FacebookPostResult::Success {
                post_id: "1_2".to_string(),
                usage: None,
            }),
        ]);
        let mut marks = FakeMarkSink::failing(vec!["fb_duplicate"]);
        let mut log = FakeLog::new();
        let queue = vec![
            notification("https://www.aiub.edu/notice/1"),
            notification("https://www.aiub.edu/notice/2"),
        ];
        let outcome = run_fb_phase(queue, false, &sender, &mut marks, &mut log);

        assert_eq!(outcome.sent_count, 1);
        assert_eq!(outcome.failed_count, 1);
        assert_eq!(
            sender.calls().len(),
            2,
            "duplicate continues to the next notice"
        );
        assert_eq!(
            marks.calls,
            vec![
                "fb_attempt:https://www.aiub.edu/notice/1",
                "fb_attempt:https://www.aiub.edu/notice/2",
                "fb_notified:https://www.aiub.edu/notice/2:1_2",
            ]
        );
        assert_eq!(
            outcome
                .fb_alerts
                .iter()
                .map(|(category, _)| category.as_str())
                .collect::<Vec<_>>(),
            vec!["duplicate_post"]
        );
        assert!(
            log.errors()
                .iter()
                .any(|m| m.contains("[Facebook Duplicate Mark Failed]"))
        );
    }

    // ── (b) FB cooldown cap, end to end ──────────────────────────────

    #[test]
    fn fb_cooldown_cap_end_to_end_regain_access_capped_at_seven_days() {
        // Medium #6: an absurd estimated_time_to_regain_access in the
        // BUC header must cap the cooldown at 7 days, end to end:
        // header -> parse -> classify -> phase step.
        let header = r#"{"1234567890":[{"type":"pages","call_count":99,"total_cputime":99,"total_time":99,"estimated_time_to_regain_access":999999}]}"#;
        let usage = parse_buc_usage_header(header).expect("valid BUC header");
        assert_eq!(
            usage.estimated_time_to_regain_access,
            FACEBOOK_BUC_MAX_COOLDOWN_MINS
        );
        let result = classify_publish_error(
            429,
            0,
            None,
            "Too Many Requests",
            "rate limited",
            Some(&usage),
        );
        let cooldown = match result {
            FacebookPostResult::RateLimited { cooldown_minutes } => cooldown_minutes,
            other => panic!("expected RateLimited, got {other:?}"),
        };
        assert_eq!(cooldown, 60 * 24 * 7);

        let wrapped: worker::Result<FacebookPostResult> = Ok(result);
        let mut outcome = DispatchOutcome::default();
        let step = facebook_send_step(&mut outcome, "https://www.aiub.edu/notice/1", &wrapped);
        assert!(matches!(step, FacebookSendStep::RateLimitedStop { .. }));
        assert_eq!(outcome.app_state_sets.len(), 1);
        assert_eq!(outcome.app_state_sets[0].0, FACEBOOK_COOLDOWN_UNTIL_KEY);
        let mins = minutes_until(&outcome.app_state_sets[0].1);
        assert!(
            (60 * 24 * 7 - 1..=60 * 24 * 7).contains(&mins),
            "cooldown must be ~7 days out, got {mins} mins"
        );
    }

    // ── (c) run-level deadline ───────────────────────────────────────

    #[test]
    fn run_with_deadline_finishes_before_deadline() {
        use crate::{DeadlineOutcome, run_with_deadline};
        use std::future::{pending, ready};
        let outcome = run_with_deadline(ready(Ok::<(), worker::Error>(())), pending::<()>())
            .now_or_never()
            .expect("deadline race completes synchronously");
        assert!(matches!(outcome, DeadlineOutcome::Finished(Ok(()))));
    }

    #[test]
    fn run_with_deadline_expires_when_run_never_finishes() {
        use crate::{DeadlineOutcome, run_with_deadline};
        use std::future::{pending, ready};
        let outcome = run_with_deadline(pending::<worker::Result<()>>(), ready(()))
            .now_or_never()
            .expect("deadline race completes synchronously");
        assert!(matches!(outcome, DeadlineOutcome::Expired));
    }

    #[test]
    fn run_with_deadline_propagates_run_error_unchanged() {
        use crate::{DeadlineOutcome, run_with_deadline};
        use std::future::{pending, ready};
        let outcome = run_with_deadline(
            ready(Err::<(), worker::Error>(worker::Error::RustError(
                "boom".into(),
            ))),
            pending::<()>(),
        )
        .now_or_never()
        .expect("deadline race completes synchronously");
        assert!(matches!(outcome, DeadlineOutcome::Finished(Err(_))));
    }

    // ── Detail analysis + listing helpers ────────────────────────────

    #[test]
    fn analyze_detail_results_counts_outcomes_and_builds_map() {
        use crate::analyze_detail_results;
        use crate::scraper::{DetailFetchOutcome, ParsedDetail};

        let detail = ParsedDetail {
            title: Some("Title".to_string()),
            published_date: None,
            body_text: "Body".to_string(),
            attachments: vec![],
            legacy_sticky_body: String::new(),
        };
        let results = vec![
            (
                "https://www.aiub.edu/notice/1".to_string(),
                DetailFetchOutcome::Success(detail),
            ),
            (
                "https://www.aiub.edu/notice/2".to_string(),
                DetailFetchOutcome::HttpError("timeout".to_string()),
            ),
            (
                "https://www.aiub.edu/notice/3".to_string(),
                DetailFetchOutcome::ParseError {
                    reason: "empty body",
                    html_len: 123,
                },
            ),
            (
                "https://www.aiub.edu/notice/4".to_string(),
                DetailFetchOutcome::CpuSkipped,
            ),
        ];
        let analysis = analyze_detail_results(results);

        assert_eq!(analysis.attempted, 4);
        assert_eq!(analysis.succeeded, 1);
        assert_eq!(analysis.http_errors, 1);
        assert_eq!(analysis.parse_errors, 1);
        assert_eq!(analysis.cpu_skipped, 1);
        assert_eq!(analysis.diagnostics.len(), 3);
        assert!(
            analysis.diagnostics[0].contains("HTTP: timeout"),
            "HTTP diagnostic names the failure"
        );
        assert!(
            analysis.diagnostics[1].contains("DOM: empty body"),
            "parse diagnostic names the reason"
        );
        assert_eq!(analysis.detail_map.len(), 4);
        assert!(
            analysis
                .detail_map
                .get("https://www.aiub.edu/notice/1")
                .unwrap()
                .is_some(),
            "successful fetch maps to its detail"
        );
        assert!(
            analysis
                .detail_map
                .get("https://www.aiub.edu/notice/2")
                .unwrap()
                .is_none(),
            "failed fetch maps to None"
        );
    }

    #[test]
    fn deduplicate_listing_notices_keeps_first_occurrence_in_order() {
        use crate::{deduplicate_listing_notices, models::Notice};
        let mk = |url: &str| Notice {
            title: url.to_string(),
            description: String::new(),
            url: url.to_string(),
            published_date: None,
            body_text: String::new(),
            attachments: vec![],
            detail_fetched: false,
        };
        let listing = vec![
            mk("https://www.aiub.edu/notice/1"),
            mk("https://www.aiub.edu/notice/2"),
            mk("https://www.aiub.edu/notice/1"),
            mk("https://www.aiub.edu/notice/3"),
            mk("https://www.aiub.edu/notice/2"),
        ];
        let unique = deduplicate_listing_notices(listing);
        assert_eq!(
            unique.iter().map(|n| n.url.as_str()).collect::<Vec<_>>(),
            vec![
                "https://www.aiub.edu/notice/1",
                "https://www.aiub.edu/notice/2",
                "https://www.aiub.edu/notice/3",
            ]
        );
    }

    #[test]
    fn listing_coverage_measures_date_and_description_fractions() {
        use crate::{listing_coverage, models::Notice};
        let mk = |dated: bool, described: bool| Notice {
            title: "T".to_string(),
            description: if described {
                "D".to_string()
            } else {
                String::new()
            },
            url: "https://www.aiub.edu/notice/x".to_string(),
            published_date: if dated {
                Some("2026-11-20".to_string())
            } else {
                None
            },
            body_text: String::new(),
            attachments: vec![],
            detail_fetched: false,
        };
        let listing = vec![
            mk(true, true),
            mk(true, false),
            mk(false, true),
            mk(false, false),
        ];
        let (date_coverage, desc_coverage) = listing_coverage(&listing);
        assert_eq!(date_coverage, 0.5);
        assert_eq!(desc_coverage, 0.5);
    }
}
