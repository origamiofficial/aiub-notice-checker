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
        let html = "<html><body><div class='notice-page'><p>No question column</p></div></body></html>";
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
        display_date, format_notice_message, titles_equivalent, truncate_text, Notice,
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
        assert!(is_meaningful_text("Classes are suspended today due to weather."));
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
            sent_at: None,
            last_notified_hash: None,
            detail_checked_at: detail_checked_at.map(String::from),
            detail_fetched_at: detail_fetched_at.map(String::from),
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
        clean_mojibake, format_detail_failure_alert, has_utf8_mojibake, titles_equivalent_clean,
    };

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
