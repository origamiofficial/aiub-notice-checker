// Unit tests for the scraper and version parsing logic.
//
// These run as `#[cfg(test)]` within the crate so they can test `pub(crate)` helpers.
// The `worker` crate's types (Fetch, etc.) are not available in a standard test harness,
// so we test the pure parsing functions that don't require the Workers runtime.

#[cfg(test)]
mod scraper_tests {
    use crate::scraper::{
        normalize_aiub_url, normalize_notice_url, parse_detail_html, parse_detail_with_reason,
        parse_listing_html, USER_AGENT,
    };

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
        let html = r#"
        <html><body>
        <div class="card">
            <a href="/notice/card-notice">Link</a>
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
        display_date, format_facebook_message, format_notice_message, titles_equivalent,
        truncate_text, Notice,
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
        assert_eq!(msg, fb_msg, "Telegram and Facebook messages must be identical");

        // Verify all parts are present in the exact unified structure
        assert!(msg.contains("Seat Plan for Final-Term Exams of Fall 2025-26"));
        assert!(msg.contains("Date: 21 Nov 2026"));
        assert!(msg.contains("Please contact Exam Control Room if your name or course is not in the list."));
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
        // When checked == fetched, the third match arm (`_ => true`) applies
        assert!(row.detail_due("2026-01-01T01:00:00Z"));
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
        clean_mojibake, format_detail_failure_alert, format_duration_human,
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
            format!("[Listing Fetch Skipped] AIUB listing page unreachable: {timeout_err}. Skipping this run.")
        );

        let status_err = "HTTP 502 when fetching https://www.aiub.edu/category/notices?pageNo=1&pageSize=50";
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
        assert!(TELEGRAM_MAX_MESSAGE_LEN > 100);

        // Facebook configuration checks
        assert_eq!(FACEBOOK_GRAPH_API_VERSION, "v27.0");
        assert!(MAX_FACEBOOK_SENDS_PER_RUN > 0 && MAX_FACEBOOK_SENDS_PER_RUN <= 10);
        assert!(FACEBOOK_PACE_MS >= 1000);
        assert!(FACEBOOK_MAX_MESSAGE_LEN >= 1000);
        assert!(FACEBOOK_BUC_USAGE_THRESHOLD_PCT >= 50 && FACEBOOK_BUC_USAGE_THRESHOLD_PCT <= 100);
        assert!(FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS > 0);
        assert!(FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS > 0);
        assert!(FACEBOOK_ADMIN_ALERT_DEBOUNCE_HOURS > 0);
    }
}

#[cfg(test)]
mod version_tests {
    use crate::version::ReleaseVersion;

    #[test]
    fn parse_legacy_xy_format() {
        let v = ReleaseVersion::parse("5.9").expect("valid version");
        assert_eq!(
            v,
            ReleaseVersion {
                major: 5,
                minor: 9,
                patch: 0
            }
        );
    }

    #[test]
    fn parse_semver_xyz_format() {
        let v = ReleaseVersion::parse("6.1.2").expect("valid version");
        assert_eq!(
            v,
            ReleaseVersion {
                major: 6,
                minor: 1,
                patch: 2
            }
        );
    }

    #[test]
    fn parse_with_v_prefix() {
        let v = ReleaseVersion::parse("v7.8.9").expect("valid version");
        assert_eq!(
            v,
            ReleaseVersion {
                major: 7,
                minor: 8,
                patch: 9
            }
        );
    }

    #[test]
    fn parse_rejects_dual_digits_for_minor_and_patch() {
        assert!(ReleaseVersion::parse("6.10.0").is_none());
        assert!(ReleaseVersion::parse("6.1.10").is_none());
    }

    #[test]
    fn single_digit_rollover_on_same_day_release() {
        // 7.8.9 same day -> 7.9.0 (Z rolls over to 0, Y increments)
        let v = ReleaseVersion::new(7, 8, 9).unwrap().next(true);
        assert_eq!(v.to_string(), "7.9.0");

        // 7.9.9 same day -> 8.0.0 (both Z and Y roll over)
        let v = ReleaseVersion::new(7, 9, 9).unwrap().next(true);
        assert_eq!(v.to_string(), "8.0.0");

        // 6.0.0 same day -> 6.0.1 (normal hotfix)
        let v = ReleaseVersion::new(6, 0, 0).unwrap().next(true);
        assert_eq!(v.to_string(), "6.0.1");
    }

    #[test]
    fn single_digit_rollover_on_new_day_release() {
        // 5.9.0 after >= 24h -> 6.0.0 (Y rolls over to 0, X increments)
        let v = ReleaseVersion::new(5, 9, 0).unwrap().next(false);
        assert_eq!(v.to_string(), "6.0.0");

        // 6.0.1 after >= 24h -> 6.1.0 (Y increments, Z resets to 0)
        let v = ReleaseVersion::new(6, 0, 1).unwrap().next(false);
        assert_eq!(v.to_string(), "6.1.0");

        // 6.9.5 after >= 24h -> 7.0.0 (Y rolls over to 0, X increments)
        let v = ReleaseVersion::new(6, 9, 5).unwrap().next(false);
        assert_eq!(v.to_string(), "7.0.0");

        // Large major version: 12.9.0 -> 13.0.0
        let v = ReleaseVersion::new(12, 9, 0).unwrap().next(false);
        assert_eq!(v.to_string(), "13.0.0");
    }
}

#[cfg(test)]
mod facebook_tests {
    use crate::config::FACEBOOK_MAX_MESSAGE_LEN;
    use crate::facebook::{classify_meta_error, parse_buc_usage_header, MetaErrorAction};
    use crate::models::{format_facebook_message, Notice};

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
            msg.chars().count() <= FACEBOOK_MAX_MESSAGE_LEN,
            "Message length {} exceeds max {}",
            msg.chars().count(),
            FACEBOOK_MAX_MESSAGE_LEN
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

    #[test]
    fn parse_buc_usage_header_malformed() {
        assert!(parse_buc_usage_header("").is_none());
        assert!(parse_buc_usage_header("not json").is_none());
        assert!(parse_buc_usage_header("[]").is_none());
        assert!(parse_buc_usage_header("{}").is_none());
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
        assert!(migration_sql
            .contains("ALTER TABLE notices ADD COLUMN fb_notification_attempted_at TEXT;"));
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
