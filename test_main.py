import logging
import os
import sqlite3
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest.mock import patch

from lxml import html

import main


class NoticeCheckerTests(unittest.TestCase):
    def database(self):
        conn = sqlite3.connect(":memory:")
        conn.row_factory = sqlite3.Row
        main.ensure_schema(conn)
        return conn

    def legacy_database(self):
        conn = sqlite3.connect(":memory:")
        conn.row_factory = sqlite3.Row
        conn.execute(
            """CREATE TABLE notices (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', link TEXT NOT NULL,
               published_date TEXT, body_text TEXT NOT NULL DEFAULT '',
               attachments_json TEXT NOT NULL DEFAULT '[]', content_hash TEXT NOT NULL DEFAULT '',
               first_seen_at TEXT NOT NULL DEFAULT '', last_seen_at TEXT NOT NULL DEFAULT '',
               sent_at TEXT, last_notified_hash TEXT)"""
        )
        conn.execute("CREATE UNIQUE INDEX idx_notices_link ON notices(link)")
        return conn

    def test_nested_listing_text_and_image_only_detail(self):
        listing = html.fromstring(
            '<div class="notification"><h2 class="title">New <em>notice</em></h2>'
            '<a class="info-link" href="/notice">Details</a>'
            '<p class="desc">Read <strong>this</strong></p>'
            '<div class="date-custom">22 Sep 2026</div></div>'
        )
        notice = main.parse_listing_page(listing)[0]
        self.assertEqual((notice.title, notice.description), ("New notice", "Read this"))
        detail = html.fromstring(
            '<h1 id="dynamicHeading">New notice</h1><div class="notice-page">'
            '<div class="question-column"><img src="/Files/Uploads/poster.png">'
            '<a href="/Files/Uploads/details.pdf">PDF</a></div></div>'
        )
        enriched = main.parse_detail_page(detail, notice)
        self.assertTrue(enriched.detail_fetched)
        self.assertIn("https://www.aiub.edu/Files/Uploads/poster.png", enriched.attachments)
        self.assertIn("https://www.aiub.edu/Files/Uploads/details.pdf", enriched.attachments)

    def test_truncated_titles_ignore_punctuation_differences(self):
        self.assertTrue(main.titles_equivalent("LL B Admission Test...", "LL.B Admission Test Results"))
        self.assertTrue(main.titles_equivalent("Time Extension Payment...", "Payment Schedule for Summer"))

    def test_truncated_listing_title_does_not_ping_pong_after_failed_refresh(self):
        conn = self.database()
        url = "https://www.aiub.edu/payment-schedule"
        cached = main.Notice(
            "Payment Schedule for Summer 2025-2026", "Description", url, "2026-09-22",
            "Cached body", [], True,
        )
        cached_hash = main.notice_hash(cached)
        main.upsert_seen_notice(conn, cached, cached_hash, detail_checked=True)
        main.mark_notified(conn, url, cached_hash)
        listing = main.Notice("Time Extension Payment Schedule for Summer ...", "Description", url, "2026-09-22")
        with patch.object(main, "enrich_notice", side_effect=[listing, cached]), patch.object(
            main, "send_telegram_message"
        ) as send:
            main.process_notices(conn, None, None, [listing], {"chat_id": "test"}, False)
            conn.execute(
                "UPDATE notices SET detail_checked_at=detail_fetched_at WHERE link=?", (url,)
            )
            main.process_notices(conn, None, None, [listing], {"chat_id": "test"}, False)
        send.assert_not_called()
        self.assertEqual(main.get_notice_row(conn, url)["title"], cached.title)
        conn.close()

    def test_empty_detail_container_is_rejected(self):
        notice = main.Notice("Notice", "Listing text", "https://www.aiub.edu/notice", "2026-09-22")
        detail = html.fromstring(
            '<h1 id="dynamicHeading">Notice</h1><div class="notice-page">'
            '<div class="question-column"></div></div>'
        )
        with self.assertRaisesRegex(ValueError, "no usable content"):
            main.parse_detail_page(detail, notice)

    def test_detail_body_excludes_embedded_page_code(self):
        notice = main.Notice("Legacy notice", "", "https://www.aiub.edu/legacy", "2013-12-19")
        detail = html.fromstring(
            '<h1 id="dynamicHeading">Legacy notice</h1><div class="notice-page">'
            '<div class="question-column"><style>.title { color: red; }</style>'
            '<script>window.alert("hidden")</script><noscript>Enable scripts</noscript>'
            '<template>Hidden template</template><p>Visible <strong>notice</strong> text.</p>'
            '</div></div>'
        )
        enriched = main.parse_detail_page(detail, notice)
        self.assertEqual(enriched.body_text, "Visible notice text.")

    def test_parser_cleanup_is_silently_rebaselined(self):
        conn = self.database()
        url = "https://www.aiub.edu/legacy-styled-notice"
        cached = main.Notice(
            "Legacy notice", "Description", url, "2013-12-19",
            ".title { color: red; } Visible notice text.",
            ["http://www.aiub.edu/"], True,
        )
        cached_hash = main.notice_hash(cached)
        main.upsert_seen_notice(conn, cached, cached_hash, detail_checked=True)
        main.mark_notified(conn, url, cached_hash)
        listing = main.Notice("Legacy notice", "Description", url, "2013-12-19")
        cleaned = main.Notice(
            "Legacy notice", "Description", url, "2013-12-19",
            "Visible notice text.", ["https://external.example/new.pdf"], True,
            cached.body_text, cached.attachments,
        )
        with patch.object(main, "enrich_notice", return_value=cleaned), patch.object(
            main, "send_telegram_message"
        ) as send:
            new_count, edited_count, failed_count = main.process_notices(
                conn, None, None, [listing], {"chat_id": "test"}, False
            )
        send.assert_not_called()
        self.assertEqual((new_count, edited_count, failed_count), (0, 0, 0))
        row = main.get_notice_row(conn, url)
        self.assertEqual(row["body_text"], cleaned.body_text)
        self.assertEqual(row["content_hash"], row["last_notified_hash"])
        conn.close()

    def test_listing_fails_when_date_parsing_systemically_breaks(self):
        posts = "".join(
            '<div class="notification"><h2 class="title">Notice</h2>'
            f'<a class="info-link" href="/notice-{index}">Details</a>'
            '<p class="desc">Details</p><div class="date-custom">not-a-date</div></div>'
            for index in range(5)
        )
        with self.assertRaisesRegex(ValueError, "date coverage"):
            main.parse_listing_page(html.fromstring(posts))

    def test_listing_fails_when_descriptions_systemically_disappear(self):
        posts = "".join(
            '<div class="notification"><h2 class="title">Notice</h2>'
            f'<a class="info-link" href="/notice-{index}">Details</a>'
            '<div class="date-custom">22 Sep 2026</div></div>'
            for index in range(5)
        )
        with self.assertRaisesRegex(ValueError, "description coverage"):
            main.parse_listing_page(html.fromstring(posts))

    def test_partial_first_crawl_is_rejected(self):
        def listing(count):
            posts = "".join(
                '<div class="notification"><h2 class="title">Notice</h2>'
                f'<a class="info-link" href="/notice-{index}">Details</a>'
                '<p class="desc">Details</p><div class="date-custom">22 Sep 2026</div></div>'
                for index in range(count)
            )
            pagination = (
                '<ul class="pagination">'
                '<li><a href="/category/notices?pageNo=1&pageSize=2">1</a></li>'
                '<li><a href="/category/notices?pageNo=2&pageSize=2">2</a></li>'
                '</ul>'
            )
            return html.fromstring(f"<html><body>{posts}{pagination}</body></html>")

        with patch.object(main, "LIST_PAGE_SIZE", 2), patch.object(
            main, "fetch_html", side_effect=[listing(2), listing(0)]
        ):
            with self.assertRaises(ValueError):
                main.crawl_notices(None, set())

    def test_cross_page_duplicate_is_rejected(self):
        def listing(urls, page):
            posts = "".join(
                '<div class="notification"><h2 class="title">Notice</h2>'
                f'<a class="info-link" href="{url}">Details</a>'
                '<p class="desc">Details</p><div class="date-custom">22 Sep 2026</div></div>'
                for url in urls
            )
            pagination = (
                '<ul class="pagination">'
                '<li><a href="/category/notices?pageNo=1&pageSize=2">1</a></li>'
                '<li><a href="/category/notices?pageNo=2&pageSize=2">2</a></li>'
                f'<li class="current">{page}</li></ul>'
            )
            return html.fromstring(f"<html><body>{posts}{pagination}</body></html>")

        with patch.object(main, "LIST_PAGE_SIZE", 2), patch.object(
            main, "fetch_html",
            side_effect=[listing(["/one", "/two"], 1), listing(["/two", "/three"], 2)],
        ):
            with self.assertRaisesRegex(ValueError, "repeated notice"):
                main.crawl_notices(None, set(), full_scan=True)

    def test_full_scan_rejects_catastrophic_archive_shrink(self):
        posts = "".join(
            '<div class="notification"><h2 class="title">Notice</h2>'
            f'<a class="info-link" href="/notice-{index}">Details</a>'
            '<p class="desc">Details</p><div class="date-custom">22 Sep 2026</div></div>'
            for index in range(4)
        )
        page = html.fromstring(
            f'<html><body>{posts}<ul class="pagination">'
            '<li><a href="/category/notices?pageNo=1&pageSize=100">1</a></li>'
            '</ul></body></html>'
        )
        known = {f"https://www.aiub.edu/notice-{index}" for index in range(100)}
        with patch.object(main, "fetch_html", return_value=page):
            with self.assertRaisesRegex(ValueError, "retained only"):
                main.crawl_notices(None, known, full_scan=True)

    def test_failed_edit_is_retried(self):
        conn = self.database()
        url = "https://www.aiub.edu/test-notice"
        old = main.Notice("Title", "Old", url, "2026-09-22", "Body", [], True)
        old_hash = main.notice_hash(old)
        main.upsert_seen_notice(conn, old, old_hash)
        main.mark_notified(conn, url, old_hash)
        changed = main.Notice("Title", "Changed", url, "2026-09-22", "Body", [], True)
        config = {"chat_id": "test", "bot_api_key": "test", "github_run_number": "1"}
        with patch.object(main, "enrich_notice", return_value=changed), patch.object(
            main, "send_telegram_message", side_effect=[False, True]
        ) as send:
            main.process_notices(conn, None, None, [changed], config, False)
            main.process_notices(conn, None, None, [changed], config, False)
        self.assertEqual(send.call_count, 2)
        row = main.get_notice_row(conn, url)
        self.assertEqual(row["content_hash"], row["last_notified_hash"])
        conn.close()

    def test_blank_body_listing_edit_is_sent(self):
        conn = self.database()
        url = "https://www.aiub.edu/blank-body"
        old = main.Notice("Title", "Old", url, "2026-09-22", "", [], True)
        old_hash = main.notice_hash(old)
        main.upsert_seen_notice(conn, old, old_hash)
        main.mark_notified(conn, url, old_hash)
        changed = main.Notice("Title", "Changed", url, "2026-09-22", "", [], True)
        config = {"chat_id": "test", "bot_api_key": "test", "github_run_number": "1"}
        with patch.object(main, "enrich_notice", return_value=changed), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, [changed], config, False)
        send.assert_called_once()
        conn.close()

    def test_listing_title_edit_is_sent_when_detail_fetch_fails(self):
        conn = self.database()
        url = "https://www.aiub.edu/title-edit"
        old = main.Notice("Old title", "Description", url, "2026-09-22")
        old_hash = main.notice_hash(old)
        main.upsert_seen_notice(conn, old, old_hash)
        main.mark_notified(conn, url, old_hash)
        changed = main.Notice("New title", "Description", url, "2026-09-22")
        with patch.object(main, "enrich_notice", return_value=changed), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, [changed], {"chat_id": "test"}, False)
        send.assert_called_once()
        conn.close()

    def test_listing_title_edit_survives_failed_refresh_after_prior_detail(self):
        conn = self.database()
        url = "https://www.aiub.edu/title-edit-with-detail"
        old = main.Notice("Old title", "Description", url, "2026-09-22", "Cached body", [], True)
        old_hash = main.notice_hash(old)
        main.upsert_seen_notice(conn, old, old_hash, detail_checked=True)
        main.mark_notified(conn, url, old_hash)
        changed = main.Notice("New title", "Description", url, "2026-09-22")
        with patch.object(main, "enrich_notice", return_value=changed), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, [changed], {"chat_id": "test"}, False)
        send.assert_called_once()
        row = main.get_notice_row(conn, url)
        self.assertEqual(row["title"], changed.title)
        self.assertEqual(row["body_text"], old.body_text)
        conn.close()

    def test_off_page_failed_send_is_retried(self):
        conn = self.database()
        config = {"chat_id": "test", "bot_api_key": "test"}
        current = main.Notice("Current", "", "https://www.aiub.edu/current", "2026-09-22")
        pending = main.Notice("Old", "", "https://www.aiub.edu/old", "2020-01-01")
        for notice in (current, pending):
            content_hash = main.notice_hash(notice)
            main.upsert_seen_notice(conn, notice, content_hash)
            if notice is current:
                main.mark_notified(conn, notice.url, content_hash)
        conn.commit()
        with patch.object(main, "enrich_notice", side_effect=lambda _session, notice: notice), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, [current], config, False)
        send.assert_called_once()
        self.assertEqual(main.get_notice_row(conn, pending.url)["content_hash"],
                         main.get_notice_row(conn, pending.url)["last_notified_hash"])
        conn.close()

    def test_off_page_retry_refreshes_detail_before_delivery(self):
        conn = self.database()
        current = main.Notice("Current", "", "https://www.aiub.edu/current", "2026-09-22")
        pending = main.Notice(
            "Pending", "", "https://www.aiub.edu/pending", "2026-09-21", "Stale body", [], True
        )
        for notice in (current, pending):
            content_hash = main.notice_hash(notice)
            main.upsert_seen_notice(conn, notice, content_hash, detail_checked=notice.detail_fetched)
            if notice is current:
                main.mark_notified(conn, notice.url, content_hash)
        for index in range(20):
            notice = main.Notice(
                f"Archive {index}", "", f"https://www.aiub.edu/archive-retry-{index}", "2020-01-01"
            )
            content_hash = main.notice_hash(notice)
            main.upsert_seen_notice(conn, notice, content_hash)
            main.mark_notified(conn, notice.url, content_hash)
        refreshed = main.Notice(
            pending.title, pending.description, pending.url, pending.published_date,
            "Fresh body", [], True,
        )
        fetched = []

        def enrich(_session, notice):
            fetched.append(notice.url)
            if notice.url == pending.url:
                return refreshed
            notice.detail_fetched = True
            return notice

        with patch.object(main, "enrich_notice", side_effect=enrich), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, [current], {"chat_id": "test"}, False)
        self.assertIn(pending.url, fetched)
        send.assert_called_once()
        self.assertIn("Fresh body", send.call_args.args[2])
        conn.close()

    def test_first_detail_backfill_with_different_listing_title_is_silent(self):
        conn = self.database()
        url = "https://www.aiub.edu/probhabok"
        stored = main.Notice("PROBHABOK 5.0", "Competition", url, "2026-09-22")
        old_hash = main.notice_hash(stored)
        main.upsert_seen_notice(conn, stored, old_hash)
        main.mark_notified(conn, url, old_hash)
        previous_position = main.get_notice_row(conn, url)["content_changed_at"]
        listing = main.Notice("PROBHABOK 5 0", "Competition", url, "2026-09-22")
        enriched = main.Notice(
            "PROBHABOK 5.0", "Competition", url, "2026-09-22", "",
            ["https://www.aiub.edu/Files/Uploads/poster.webp"], True,
        )
        with patch.object(main, "enrich_notice", return_value=enriched), patch.object(
            main, "send_telegram_message"
        ) as send:
            main.process_notices(conn, None, None, [listing], {"chat_id": "test"}, False)
        send.assert_not_called()
        row = main.get_notice_row(conn, url)
        self.assertEqual(row["content_hash"], row["last_notified_hash"])
        self.assertEqual(row["content_changed_at"], previous_position)
        self.assertIn("poster.webp", row["attachments_json"])
        conn.close()

    def test_legacy_detail_migration_backfills_without_notification(self):
        conn = self.legacy_database()
        populated = main.Notice(
            "Legacy", "Description", "https://www.aiub.edu/legacy", "2026-09-22",
            "Cached body", ["https://www.aiub.edu/old.pdf", "http://www.aiub.edu/"],
        )
        blank = main.Notice("Blank", "", "https://www.aiub.edu/blank", "2026-09-21")
        for notice in (populated, blank):
            content_hash = main.notice_hash(notice)
            conn.execute(
                """INSERT INTO notices
                   (title, description, link, published_date, body_text, attachments_json,
                    content_hash, first_seen_at, last_seen_at, sent_at, last_notified_hash)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                (notice.title, notice.description, notice.url, notice.published_date,
                 notice.body_text, notice.attachments_json, content_hash,
                 "2026-09-22T00:00:00+00:00", "2026-09-22T00:00:00+00:00",
                 "2026-09-22T00:00:00+00:00", content_hash),
            )
        main.ensure_schema(conn)
        self.assertIsNone(main.get_notice_row(conn, populated.url)["detail_fetched_at"])
        self.assertIsNone(main.get_notice_row(conn, blank.url)["detail_fetched_at"])
        enriched = main.Notice(
            populated.title, populated.description, populated.url, populated.published_date,
            populated.body_text,
            ["https://www.aiub.edu/old.pdf", "https://external.example/new"], True,
        )

        def enrich(_session, notice):
            return enriched if notice.url == populated.url else notice

        with patch.object(main, "enrich_notice", side_effect=enrich), patch.object(
            main, "send_telegram_message"
        ) as send:
            main.process_notices(conn, None, None, [populated], {"chat_id": "test"}, False)
        send.assert_not_called()
        row = main.get_notice_row(conn, populated.url)
        self.assertEqual(row["content_hash"], row["last_notified_hash"])
        self.assertIsNotNone(row["detail_fetched_at"])
        self.assertIn("external.example", row["attachments_json"])
        conn.close()

    def test_legacy_detail_migration_still_notifies_real_body_edit(self):
        conn = self.legacy_database()
        old = main.Notice(
            "Legacy", "Description", "https://www.aiub.edu/legacy-edit", "2026-09-22",
            "Original body", ["https://www.aiub.edu/details.pdf"],
        )
        old_hash = main.notice_hash(old)
        conn.execute(
            """INSERT INTO notices
               (title, description, link, published_date, body_text, attachments_json,
                content_hash, first_seen_at, last_seen_at, sent_at, last_notified_hash)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (old.title, old.description, old.url, old.published_date, old.body_text,
             old.attachments_json, old_hash, "2026-09-22T00:00:00+00:00",
             "2026-09-22T00:00:00+00:00", "2026-09-22T00:00:00+00:00", old_hash),
        )
        main.ensure_schema(conn)
        changed = main.Notice(
            old.title, old.description, old.url, old.published_date,
            "Materially changed body", old.attachments, True,
        )
        with patch.object(main, "enrich_notice", return_value=changed), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, [old], {"chat_id": "test"}, False)
        send.assert_called_once()
        row = main.get_notice_row(conn, old.url)
        self.assertEqual(row["content_hash"], row["last_notified_hash"])
        self.assertEqual(row["body_text"], changed.body_text)
        conn.close()

    def test_first_run_rss_keeps_newest_publication_first(self):
        conn = self.database()
        newest = main.Notice("Newest", "", "https://www.aiub.edu/newest", "2026-09-22")
        oldest = main.Notice("Oldest", "", "https://www.aiub.edu/oldest", "2020-01-01")
        with patch.object(main, "enrich_notice", side_effect=lambda _session, notice: notice):
            main.process_notices(conn, None, None, [newest, oldest], {"chat_id": "test"}, True)
        with tempfile.TemporaryDirectory() as directory:
            feed_path = Path(directory) / "rss.xml"
            with patch.object(main, "RSS_FEED_FILE", str(feed_path)):
                main.generate_rss_feed(conn)
            links = [item.findtext("link") for item in ET.parse(feed_path).getroot().findall("channel/item")]
        self.assertEqual(links, [newest.url, oldest.url])
        conn.close()

    def test_failed_detail_get_is_retried_before_unchecked_archive(self):
        conn = self.database()
        current = main.Notice("Current", "", "https://www.aiub.edu/current", "2026-09-22")
        failed = main.Notice("Failed", "", "https://www.aiub.edu/failed", "2026-01-01")
        archive = [
            main.Notice(str(index), "", f"https://www.aiub.edu/archive-{index}", "2020-01-01")
            for index in range(20)
        ]
        for notice in [current, failed, *archive]:
            content_hash = main.notice_hash(notice)
            main.upsert_seen_notice(conn, notice, content_hash)
            main.mark_notified(conn, notice.url, content_hash)
        conn.execute(
            "UPDATE notices SET detail_checked_at=? WHERE link=?",
            ("2020-01-01T00:00:00+00:00", failed.url),
        )
        conn.commit()
        fetched = []

        def record_fetch(_session, notice):
            fetched.append(notice.url)
            notice.detail_fetched = True
            return notice

        with patch.object(main, "enrich_notice", side_effect=record_fetch), patch.object(
            main, "send_telegram_message"
        ) as send:
            main.process_notices(conn, None, None, [current], {"chat_id": "test"}, False)
        self.assertIn(failed.url, fetched)
        send.assert_not_called()
        conn.close()

    def test_many_new_notices_defer_delivery_after_run_budget(self):
        conn = self.database()
        notices = [
            main.Notice(str(index), "", f"https://www.aiub.edu/new-{index}", "2026-09-22")
            for index in range(30)
        ]
        def successful_detail(_session, notice):
            notice.detail_fetched = True
            return notice

        with patch.object(main, "enrich_notice", side_effect=successful_detail), patch.object(
            main, "send_telegram_message", return_value=True
        ) as send:
            main.process_notices(conn, None, None, notices, {"chat_id": "test"}, False)
        pending = conn.execute("SELECT COUNT(*) FROM notices WHERE sent_at IS NULL").fetchone()[0]
        self.assertGreater(send.call_count, 0)
        self.assertLessEqual(send.call_count, 20)
        self.assertEqual(pending, 30 - send.call_count)
        conn.close()

    def test_systemic_detail_parser_failure_fails_the_run(self):
        conn = self.database()
        notices = [
            main.Notice(str(index), "", f"https://www.aiub.edu/notice-{index}", "2026-09-22")
            for index in range(5)
        ]
        with patch.object(main, "enrich_notice", side_effect=lambda _session, notice: notice):
            with self.assertRaises(main.DetailFetchHealthError):
                main.process_notices(conn, None, None, notices, {"chat_id": "test"}, False)
        conn.close()

    def test_long_telegram_retry_after_defers_without_sleeping(self):
        class Response:
            status_code = 429
            headers = {}

            def json(self):
                return {
                    "ok": False,
                    "error_code": 429,
                    "parameters": {"retry_after": 120},
                }

        class Session:
            def post(self, *args, **kwargs):
                return Response()

        main._telegram_delivery_blocked = False
        with patch.object(main, "DRY_RUN", False), patch.object(main.time, "sleep") as sleep:
            delivered = main.send_telegram_message(
                Session(), "chat", "message", {"bot_api_key": "token"}, "test"
            )
        self.assertFalse(delivered)
        self.assertTrue(main._telegram_delivery_blocked)
        sleep.assert_not_called()
        main._telegram_delivery_blocked = False

    def test_debug_logging_does_not_expose_telegram_request_path(self):
        logger = logging.getLogger("urllib3.connectionpool")
        previous = logger.level
        try:
            with patch.dict(os.environ, {"LOG_LEVEL": "DEBUG"}), patch.object(logging, "basicConfig"):
                main.configure_logging()
            self.assertGreaterEqual(logger.level, logging.INFO)
        finally:
            logger.setLevel(previous)

    def test_message_and_url_length_limits(self):
        base = "https://www.aiub.edu/"
        url = base + "a" * (main.MAX_URL_LENGTH - len(base))
        notice = main.Notice("T" * 1000, "D" * 5000, url, "2026-09-22")
        message = main.format_notice_message(notice, "12345")
        self.assertLessEqual(len(message), 4096)
        self.assertTrue(message.endswith(url + "#12345"))
        self.assertEqual(main.normalize_url(url + "x"), "")

    def test_message_sanitizes_github_run_number(self):
        notice = main.Notice("Title", "Description", "https://www.aiub.edu/notice", "2026-09-22")
        message = main.format_notice_message(notice, "12/../ bad")
        self.assertTrue(message.endswith("https://www.aiub.edu/notice#12..bad"))

    def test_dry_run_does_not_change_database_or_feed(self):
        with tempfile.TemporaryDirectory() as directory:
            db_path = Path(directory) / "notices.db"
            feed_path = Path(directory) / "rss.xml"
            feed_path.write_text("unchanged", encoding="utf-8")
            conn = sqlite3.connect(db_path)
            conn.row_factory = sqlite3.Row
            main.ensure_schema(conn)
            old = main.Notice("Title", "Old", "https://www.aiub.edu/notice", "2026-09-22")
            old_hash = main.notice_hash(old)
            main.upsert_seen_notice(conn, old, old_hash)
            main.mark_notified(conn, old.url, old_hash)
            conn.close()
            original_db = db_path.read_bytes()
            changed = main.Notice("Title", "Changed", old.url, old.published_date, "Body", [], True)
            with patch.object(main, "DB_NAME", str(db_path)), patch.object(
                main, "RSS_FEED_FILE", str(feed_path)
            ), patch.object(main, "DRY_RUN", True), patch.object(
                main, "create_aiub_session", return_value=None
            ), patch.object(main, "create_telegram_session", return_value=None), patch.object(
                main, "crawl_notices", return_value=[changed]
            ), patch.object(main, "enrich_notice", return_value=changed):
                self.assertEqual(main.main(), 0)
            self.assertEqual(db_path.read_bytes(), original_db)
            self.assertEqual(feed_path.read_text(encoding="utf-8"), "unchanged")

    def test_dry_run_does_not_create_state_files(self):
        with tempfile.TemporaryDirectory() as directory:
            db_path = Path(directory) / "notices.db"
            feed_path = Path(directory) / "rss.xml"
            notice = main.Notice("Title", "Description", "https://www.aiub.edu/notice", "2026-09-22")
            with patch.object(main, "DB_NAME", str(db_path)), patch.object(
                main, "RSS_FEED_FILE", str(feed_path)
            ), patch.object(main, "DRY_RUN", True), patch.object(
                main, "create_aiub_session", return_value=None
            ), patch.object(main, "create_telegram_session", return_value=None), patch.object(
                main, "crawl_notices", return_value=[notice]
            ), patch.object(main, "enrich_notice", return_value=notice):
                self.assertEqual(main.main(), 0)
            self.assertFalse(db_path.exists())
            self.assertFalse(feed_path.exists())

    def test_configuration_logs_do_not_expose_chat_ids(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(
            os.environ,
            {"TELEGRAM_CHAT_ID": "private-main-id", "TELEGRAM_ADMIN_CHAT_ID": "private-admin-id"},
        ), patch.object(main, "DB_NAME", str(Path(directory) / "state.db")), patch.object(
            main, "DRY_RUN", True
        ), patch.object(main, "create_aiub_session", return_value=None), patch.object(
            main, "create_telegram_session", return_value=None
        ), patch.object(main, "crawl_notices", side_effect=ValueError("expected test failure")), patch.object(
            main, "send_telegram_message", return_value=True
        ), self.assertLogs(level="INFO") as logs:
            self.assertEqual(main.main(), 1)
        output = "\n".join(logs.output)
        self.assertNotIn("private-main-id", output)
        self.assertNotIn("private-admin-id", output)

    def test_dry_run_message_log_does_not_expose_chat_id(self):
        with patch.object(main, "DRY_RUN", True), self.assertLogs(level="INFO") as logs:
            self.assertTrue(
                main.send_telegram_message(
                    None, "private-main-id", "message", {"bot_api_key": "dry-run"}, "notice"
                )
            )
        self.assertNotIn("private-main-id", "\n".join(logs.output))

    def test_telegram_checks_api_result(self):
        class Response:
            status_code = 200
            text = '{"ok": false}'

            def json(self):
                return {"ok": False, "description": "not delivered"}

        class Session:
            def post(self, *args, **kwargs):
                return Response()

        with patch.object(main, "DRY_RUN", False):
            delivered = main.send_telegram_message(
                Session(), "chat", "message", {"bot_api_key": "token"}, "test"
            )
        self.assertFalse(delivered)

    def test_rss_keeps_stable_guid_and_shows_links(self):
        conn = self.database()
        url = "https://www.aiub.edu/test-notice"
        pdf = "https://www.aiub.edu/Files/Uploads/details.pdf"
        notice = main.Notice("Title", "Description", url, "2026-09-22", "", [pdf], True)
        main.upsert_seen_notice(conn, notice, main.notice_hash(notice))
        with tempfile.TemporaryDirectory() as directory:
            feed_path = Path(directory) / "rss.xml"
            with patch.object(main, "RSS_FEED_FILE", str(feed_path)):
                main.generate_rss_feed(conn)
            channel = ET.parse(feed_path).getroot().find("channel")
        self.assertEqual(channel.findtext("item/guid"), url)
        self.assertIn(pdf, channel.findtext("item/description"))
        self.assertIsNotNone(channel.findtext("lastBuildDate"))
        conn.close()


if __name__ == "__main__":
    unittest.main()
