from __future__ import annotations

import datetime as dt
import hashlib
import html as html_lib
import json
import logging
import os
import re
import sqlite3
import tempfile
import time
from collections.abc import Mapping
from contextlib import closing
from dataclasses import dataclass
from email.utils import format_datetime
from urllib.parse import parse_qs, urldefrag, urljoin, urlparse

import requests
from lxml import etree
from lxml import html as lxml_html
from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry


BASE_URL = "https://www.aiub.edu"
BASE_HOST = urlparse(BASE_URL).netloc.lower()
NOTICE_PATH = "/category/notices"

LIST_PAGE_SIZE = 100
MAX_LIST_PAGES = 60
MIN_SCAN_PAGES = 2
MIN_LISTING_DATE_COVERAGE = 0.8
MIN_LISTING_DESCRIPTION_COVERAGE = 0.8
MIN_FULL_SCAN_KNOWN_COVERAGE = 0.8
MAX_URL_LENGTH = 2048
REQUEST_TIMEOUT = (5.0, 30.0)
DETAIL_FETCH_LIMIT = LIST_PAGE_SIZE
DRY_RUN = os.environ.get("DRY_RUN", "false").lower() in {"1", "true", "yes"}
TELEGRAM_MAX_RETRIES = 3
TELEGRAM_RETRY_BACKOFF = 2.0
TELEGRAM_MAX_RETRY_DELAY = 60.0
TELEGRAM_SEND_LIMIT = 20
DETAIL_FAILURE_MIN_SAMPLE = 5
DETAIL_FAILURE_RATIO = 0.8

DB_NAME = "aiub_notices.db"
DB_TIMEOUT = 30.0
RSS_FEED_FILE = "rss.xml"
RSS_ITEM_LIMIT = 500
SCRIPT_VERSION = "5.7"

# How much of a dry-run "would send" message to preview in the logs.
DRY_RUN_PREVIEW_CHARS = 300

POST_XPATH = (
    "//div[contains(concat(' ', normalize-space(@class), ' '), ' notification ') "
    "and not(ancestor::div[contains(concat(' ', normalize-space(@class), ' '), ' notification ')])]"
)
TITLE_XPATH = ".//h2[contains(concat(' ', normalize-space(@class), ' '), ' title ')]//text()"
LINK_XPATH = ".//a[contains(concat(' ', normalize-space(@class), ' '), ' info-link ')]/@href"
DESCRIPTION_XPATH = ".//p[contains(concat(' ', normalize-space(@class), ' '), ' desc ')]//text()"
DATE_TEXT_XPATH = ".//div[contains(concat(' ', normalize-space(@class), ' '), ' date-custom ')]//text()[normalize-space()]"
PAGINATION_XPATH = "//ul[contains(concat(' ', normalize-space(@class), ' '), ' pagination ')]//a/@href"
DETAIL_TITLE_XPATH = "string(//h1[@id='dynamicHeading'])"
DETAIL_BODY_XPATH = (
    "//div[contains(concat(' ', normalize-space(@class), ' '), ' notice-page ')]"
    "//div[contains(concat(' ', normalize-space(@class), ' '), ' question-column ') "
    "and not(contains(concat(' ', normalize-space(@class), ' '), ' notice-sticky-header '))]"
)
VISIBLE_DETAIL_TEXT_XPATH = (
    ".//text()[not(ancestor::script) and not(ancestor::style) "
    "and not(ancestor::noscript) and not(ancestor::template)]"
)


@dataclass
class Notice:
    title: str
    description: str
    url: str
    published_date: str | None
    body_text: str = ""
    attachments: list[str] | None = None
    detail_fetched: bool = False
    legacy_body_text: str | None = None
    legacy_attachments: list[str] | None = None

    @property
    def attachments_json(self) -> str:
        return json.dumps(self.attachments or [], ensure_ascii=False, sort_keys=True)


def configure_logging() -> None:
    # During dry runs, default to DEBUG so every per-notice decision line
    # (including [UNCHANGED]) is visible. Can still be overridden via LOG_LEVEL.
    default_level = "DEBUG" if DRY_RUN else "INFO"
    logging.basicConfig(
        level=os.environ.get("LOG_LEVEL", default_level),
        format="%(asctime)s %(levelname)s %(message)s",
    )
    logging.getLogger("urllib3.connectionpool").setLevel(logging.WARNING)


def clean_text(text: str) -> str:
    return re.sub(r"\s+", " ", html_lib.unescape(str(text))).strip()


def titles_equivalent(listing_title: str, cached_title: str) -> bool:
    listing_key = re.sub(r"\W+", "", listing_title).casefold()
    cached_key = re.sub(r"\W+", "", cached_title).casefold()
    if listing_key == cached_key:
        return True
    # A truncated listing cannot prove that the canonical detail title changed.
    return "..." in listing_title


def normalize_url(href: str, base_url: str = BASE_URL + "/", allow_external: bool = False) -> str:
    href = (href or "").replace("\\", "/").strip()
    if not href:
        return ""
    absolute = urljoin(base_url, href)
    absolute, _fragment = urldefrag(absolute)
    if len(absolute) > MAX_URL_LENGTH:
        logging.warning("Ignoring URL longer than %s characters", MAX_URL_LENGTH)
        return ""
    parsed = urlparse(absolute)
    if parsed.scheme not in {"http", "https"}:
        logging.warning("Ignoring URL with unsupported scheme: %r", href)
        return ""
    if not parsed.hostname or parsed.username or parsed.password:
        logging.warning("Ignoring malformed URL: %s", absolute)
        return ""
    if parsed.netloc.lower() == BASE_HOST and parsed.scheme == "http":
        absolute = parsed._replace(scheme="https").geturl()
    elif parsed.netloc.lower() != BASE_HOST and not allow_external:
        logging.warning("Ignoring external URL: %s", absolute)
        return ""
    return absolute


def normalize_legacy_attachment_url(href: str) -> str:
    """Reproduce the deployed parser's URL handling for a silent one-time rebaseline."""
    href = (href or "").replace("\\", "/").strip()
    if not href:
        return ""
    absolute, _fragment = urldefrag(urljoin(BASE_URL + "/", href))
    parsed = urlparse(absolute)
    if parsed.scheme not in {"http", "https"} or parsed.netloc.lower() != BASE_HOST:
        return ""
    return absolute


def parse_date_parts(parts: list[str]) -> str | None:
    text = re.sub(r"\bSept\b", "Sep", clean_text(" ".join(parts)), flags=re.IGNORECASE)
    if not text:
        return None

    for date_format in ("%d %b %Y", "%d %B %Y"):
        try:
            return dt.datetime.strptime(text, date_format).date().isoformat()
        except ValueError:
            pass
    logging.warning("Could not parse notice date from parts: %s", text)
    return None


def display_date(iso_date: str | None) -> str:
    if not iso_date:
        return "Unknown"
    try:
        return dt.date.fromisoformat(iso_date).strftime("%d %b %Y")
    except ValueError:
        return iso_date


def create_aiub_session() -> requests.Session:
    session = requests.Session()
    session.headers.update(
        {
            "User-Agent": "aiub-notice-checker/5.0 (+https://github.com/origamiofficial/aiub-notice-checker)",
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        }
    )
    retry_options = {
        "total": 4,
        "connect": 4,
        "read": 4,
        "status": 4,
        "backoff_factor": 1.0,
        "status_forcelist": (429, 500, 502, 503, 504),
        "respect_retry_after_header": True,
    }
    retry = Retry(allowed_methods=frozenset(["GET"]), **retry_options)
    adapter = HTTPAdapter(max_retries=retry, pool_connections=10, pool_maxsize=10)
    session.mount("https://", adapter)
    session.mount("http://", adapter)
    return session


def create_telegram_session() -> requests.Session:
    session = requests.Session()
    session.headers.update({"User-Agent": "aiub-notice-checker/5.0"})
    adapter = HTTPAdapter(max_retries=Retry(total=0))
    session.mount("https://", adapter)
    session.mount("http://", adapter)
    return session


def fetch_html(session: requests.Session, url: str) -> lxml_html.HtmlElement:
    target = normalize_url(url)
    if not target or urlparse(target).scheme != "https":
        raise ValueError(f"Unsafe AIUB URL: {url}")
    for _ in range(5):
        logging.debug("HTTP GET %s", target)
        response = session.get(target, timeout=REQUEST_TIMEOUT, allow_redirects=False)
        if 300 <= response.status_code < 400:
            target = normalize_url(response.headers.get("Location", ""), target)
            if not target or urlparse(target).scheme != "https":
                raise ValueError(f"Unsafe AIUB redirect from {url}")
            continue
        response.raise_for_status()
        content_type = response.headers.get("Content-Type", "").split(";", 1)[0].strip().lower()
        if content_type not in {"text/html", "application/xhtml+xml"}:
            raise ValueError(f"Unexpected content type for {target}: {content_type or 'missing'}")
        logging.debug("HTTP %s for %s (%s bytes)", response.status_code, target, len(response.content))
        return lxml_html.fromstring(response.content, base_url=target)
    raise ValueError(f"Too many AIUB redirects: {url}")


def parse_listing_page(tree: lxml_html.HtmlElement) -> list[Notice]:
    notices: list[Notice] = []
    posts = tree.xpath(POST_XPATH)
    if not posts:
        raise ValueError("AIUB listing has no notice cards")
    for post in posts:
        title = clean_text(" ".join(post.xpath(TITLE_XPATH)))
        description = clean_text(" ".join(post.xpath(DESCRIPTION_XPATH)))
        link = "".join(post.xpath(LINK_XPATH)).strip()
        url = normalize_url(link)
        date_parts = [text for part in post.xpath(DATE_TEXT_XPATH) if (text := clean_text(part))]

        if not title or not url:
            raise ValueError(f"AIUB listing has a notice without a valid title or link: {title!r}")

        notices.append(
            Notice(
                title=title,
                description=description,
                url=url,
                published_date=parse_date_parts(date_parts),
                attachments=[],
            )
        )
    dated_count = sum(notice.published_date is not None for notice in notices)
    if len(notices) >= 5 and dated_count / len(notices) < MIN_LISTING_DATE_COVERAGE:
        raise ValueError(
            f"AIUB listing date coverage dropped to {dated_count}/{len(notices)} notices"
        )
    described_count = sum(bool(notice.description) for notice in notices)
    if (len(notices) >= 5
            and described_count / len(notices) < MIN_LISTING_DESCRIPTION_COVERAGE):
        raise ValueError(
            f"AIUB listing description coverage dropped to {described_count}/{len(notices)} notices"
        )
    return notices


def parse_detail_page(tree: lxml_html.HtmlElement, fallback: Notice) -> Notice:
    title = clean_text(tree.xpath(DETAIL_TITLE_XPATH))
    body_nodes = tree.xpath(DETAIL_BODY_XPATH)
    if not title or not body_nodes:
        raise ValueError(f"AIUB detail page structure changed: {fallback.url}")
    body_parts = []
    legacy_body_parts = []
    for node in body_nodes:
        text = clean_text(" ".join(node.xpath(VISIBLE_DETAIL_TEXT_XPATH)))
        if text:
            body_parts.append(text)
        legacy_text = clean_text(node.text_content())
        if legacy_text:
            legacy_body_parts.append(legacy_text)
    body_text = "\n\n".join(body_parts)
    legacy_body_text = "\n\n".join(legacy_body_parts)

    attachments: list[str] = []
    legacy_attachments: list[str] = []
    for node in body_nodes:
        for href in node.xpath(".//a[@href]/@href"):
            normalized = normalize_legacy_attachment_url(href)
            if normalized and normalized not in legacy_attachments:
                legacy_attachments.append(normalized)
        for href in node.xpath(".//a[@href]/@href | .//img[@src]/@src | .//img[@data-src]/@data-src"):
            if href.strip().startswith("#"):
                continue
            normalized = normalize_url(href, tree.base_url or fallback.url, allow_external=True)
            if normalized and normalized.rstrip("/") not in {BASE_URL, fallback.url.rstrip("/")} and normalized not in attachments:
                attachments.append(normalized)
    if not body_text and not attachments:
        raise ValueError(f"AIUB detail page has no usable content: {fallback.url}")

    return Notice(
        title=title,
        description=fallback.description,
        url=fallback.url,
        published_date=fallback.published_date,
        body_text=body_text,
        attachments=attachments,
        detail_fetched=True,
        legacy_body_text=legacy_body_text,
        legacy_attachments=legacy_attachments,
    )


def enrich_notice(session: requests.Session, notice: Notice) -> Notice:
    try:
        tree = fetch_html(session, notice.url)
        enriched = parse_detail_page(tree, notice)
        logging.debug(
            "Enriched %s: body_len=%s attachments=%s",
            notice.url, len(enriched.body_text), len(enriched.attachments or []),
        )
        return enriched
    except (requests.RequestException, etree.ParserError, ValueError) as exc:
        logging.warning("Could not fetch notice detail %s: %s", notice.url, exc)
        return notice


def crawl_notices(session: requests.Session, known_links: set[str], full_scan: bool = False) -> list[Notice]:
    notices: list[Notice] = []
    seen_links: set[str] = set()
    seen_page_fingerprints: set[tuple[str, ...]] = set()
    expected_last_page: int | None = None

    for page_no in range(1, MAX_LIST_PAGES + 1):
        url = f"{BASE_URL}{NOTICE_PATH}?pageNo={page_no}&pageSize={LIST_PAGE_SIZE}"
        logging.info("Fetching listing page %s: %s", page_no, url)
        tree = fetch_html(session, url)
        page_notices = parse_listing_page(tree)
        logging.info("Page %s returned %s notices.", page_no, len(page_notices))

        page_numbers = [
            int(value)
            for href in tree.xpath(PAGINATION_XPATH)
            if urlparse(urljoin(BASE_URL, href)).path == NOTICE_PATH
            for value in parse_qs(urlparse(href).query).get("pageNo", [])
            if value.isdecimal()
        ]
        if not page_numbers or page_no not in page_numbers:
            raise ValueError(f"AIUB listing pagination is missing page {page_no}")
        last_page = max(page_numbers)
        if last_page > MAX_LIST_PAGES:
            raise ValueError(f"AIUB listing exceeds the {MAX_LIST_PAGES}-page crawl limit")
        if expected_last_page is None:
            expected_last_page = last_page
        elif last_page != expected_last_page:
            raise ValueError(f"AIUB listing page count changed during crawl: {expected_last_page} to {last_page}")
        if page_no < last_page and len(page_notices) != LIST_PAGE_SIZE:
            raise ValueError(f"AIUB listing page {page_no} is incomplete: {len(page_notices)} notices")

        fingerprint = tuple(notice.url for notice in page_notices)
        if fingerprint in seen_page_fingerprints:
            raise ValueError(f"AIUB listing page {page_no} repeated a previous result")
        seen_page_fingerprints.add(fingerprint)

        page_has_new_link = False
        new_on_page = 0
        for notice in page_notices:
            if notice.url in seen_links:
                raise ValueError(f"AIUB listing repeated notice across pages: {notice.url}")
            seen_links.add(notice.url)
            notices.append(notice)
            if notice.url not in known_links:
                page_has_new_link = True
                new_on_page += 1
        logging.info("Page %s: %s links not previously known to the DB.", page_no, new_on_page)

        if page_no == last_page:
            break
        if known_links and not full_scan and page_no >= MIN_SCAN_PAGES and not page_has_new_link:
            logging.info("Stopping at page %s because all links on the page are already known.", page_no)
            break

    if full_scan and known_links:
        known_overlap = len(seen_links & known_links)
        if known_overlap / len(known_links) < MIN_FULL_SCAN_KNOWN_COVERAGE:
            raise ValueError(
                f"AIUB full scan retained only {known_overlap}/{len(known_links)} known notices"
            )
    logging.info("Crawl finished: %s total unique notices collected across all pages.", len(notices))
    return notices


def connect_db() -> sqlite3.Connection:
    conn = sqlite3.connect(DB_NAME, timeout=DB_TIMEOUT)
    conn.row_factory = sqlite3.Row
    conn.execute(f"PRAGMA busy_timeout={int(DB_TIMEOUT * 1000)}")
    return conn


def connect_run_db() -> sqlite3.Connection:
    if not DRY_RUN:
        return connect_db()
    conn = sqlite3.connect(":memory:")
    conn.row_factory = sqlite3.Row
    if os.path.exists(DB_NAME):
        # The source is opened read-only so preview runs cannot modify tracked state.
        source = sqlite3.connect(f"file:{os.path.abspath(DB_NAME)}?mode=ro", uri=True)
        try:
            source.backup(conn)
        finally:
            source.close()
    return conn


def ensure_schema(conn: sqlite3.Connection) -> None:
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS notices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            link TEXT NOT NULL,
            published_date TEXT,
            body_text TEXT NOT NULL DEFAULT '',
            attachments_json TEXT NOT NULL DEFAULT '[]',
            content_hash TEXT NOT NULL DEFAULT '',
            first_seen_at TEXT NOT NULL DEFAULT '',
            last_seen_at TEXT NOT NULL DEFAULT '',
            sent_at TEXT,
            last_notified_hash TEXT,
            detail_checked_at TEXT,
            detail_fetched_at TEXT,
            notification_attempted_at TEXT,
            content_changed_at TEXT NOT NULL DEFAULT ''
        )
        """
    )
    columns = {row["name"] for row in conn.execute("PRAGMA table_info(notices)")}
    for column, definition in {
        "detail_checked_at": "TEXT",
        "detail_fetched_at": "TEXT",
        "notification_attempted_at": "TEXT",
        "content_changed_at": "TEXT NOT NULL DEFAULT ''",
    }.items():
        if column not in columns:
            conn.execute(f"ALTER TABLE notices ADD COLUMN {column} {definition}")
    conn.execute(
        """UPDATE notices SET content_changed_at=CASE
           WHEN date(published_date) IS NOT NULL THEN published_date || 'T00:00:00+00:00'
           ELSE COALESCE(NULLIF(first_seen_at, ''), NULLIF(last_seen_at, ''), ?)
           END WHERE content_changed_at=''""",
        (utc_now(),),
    )
    # Legacy detail data predates parser-version tracking. Leaving these timestamps
    # empty makes the first refresh a silent backfill instead of a false edit.
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_notices_link ON notices(link)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_notices_published_date ON notices(published_date)")
    conn.execute("CREATE TABLE IF NOT EXISTS app_state (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    conn.commit()


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def compute_hash(
    title: str,
    description: str,
    link: str,
    published_date: str | None,
    body_text: str,
    attachments_json: str,
) -> str:
    payload = json.dumps(
        {
            "title": clean_text(title),
            "description": clean_text(description),
            "link": normalize_url(link),
            "published_date": published_date or "",
            "body_text": clean_text(body_text),
            "attachments": parse_attachments_json(attachments_json),
        },
        ensure_ascii=False,
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def notice_hash(notice: Notice) -> str:
    return compute_hash(
        notice.title,
        notice.description,
        notice.url,
        notice.published_date,
        notice.body_text,
        notice.attachments_json,
    )


def load_existing_notice_rows(conn: sqlite3.Connection) -> dict[str, sqlite3.Row]:
    rows = {
        link: row
        for row in conn.execute("SELECT * FROM notices")
        if (link := normalize_url(row["link"]))
    }
    logging.info("Loaded %s existing notices from the database.", len(rows))
    return rows


def get_notice_row(conn: sqlite3.Connection, url: str) -> sqlite3.Row | None:
    return conn.execute("SELECT * FROM notices WHERE link=?", (normalize_url(url),)).fetchone()


def parse_attachments_json(value: str | None) -> list[str]:
    if not value:
        return []
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        logging.warning("Ignoring invalid attachments_json value: %r", value)
        return []
    if not isinstance(parsed, list):
        return []
    return [str(item) for item in parsed]


def merge_cached_notice(listing_notice: Notice, existing: sqlite3.Row | None) -> Notice:
    if existing is None:
        return listing_notice

    cached_body = existing["body_text"] or ""
    cached_attachments = parse_attachments_json(existing["attachments_json"])
    cached_title = existing["title"] or ""
    title = listing_notice.title
    if cached_title and titles_equivalent(listing_notice.title, cached_title):
        title = cached_title
    return Notice(
        title=title,
        description=listing_notice.description,
        url=listing_notice.url,
        published_date=listing_notice.published_date or existing["published_date"],
        body_text=cached_body,
        attachments=cached_attachments,
    )


def upsert_seen_notice(
    conn: sqlite3.Connection,
    notice: Notice,
    content_hash: str,
    existing: sqlite3.Row | None = None,
    detail_checked: bool = False,
    historical: bool = False,
    backfill: bool = False,
) -> None:
    now = utc_now()
    if existing is None:
        changed_at = (
            f"{notice.published_date}T00:00:00+00:00"
            if historical and notice.published_date else now
        )
        conn.execute(
            """INSERT INTO notices
               (title, description, link, published_date, body_text, attachments_json,
                content_hash, first_seen_at, last_seen_at, content_changed_at,
                detail_checked_at, detail_fetched_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (notice.title, notice.description, notice.url, notice.published_date,
             notice.body_text, notice.attachments_json, content_hash, now, now, changed_at,
             now if detail_checked else None, now if notice.detail_fetched else None),
        )
    elif existing["content_hash"] != content_hash:
        conn.execute(
            """UPDATE notices SET title=?, description=?, published_date=?, body_text=?,
               attachments_json=?, content_hash=?, last_seen_at=?, content_changed_at=?,
               notification_attempted_at=NULL,
               detail_checked_at=COALESCE(?, detail_checked_at),
               detail_fetched_at=COALESCE(?, detail_fetched_at) WHERE link=?""",
            (notice.title, notice.description, notice.published_date, notice.body_text,
             notice.attachments_json, content_hash, now,
             existing["content_changed_at"] if backfill else now,
             now if detail_checked else None, now if notice.detail_fetched else None,
             notice.url),
        )
    elif detail_checked:
        conn.execute(
            """UPDATE notices SET detail_checked_at=?,
               detail_fetched_at=COALESCE(?, detail_fetched_at) WHERE link=?""",
            (now, now if notice.detail_fetched else None, notice.url),
        )


def mark_notified(conn: sqlite3.Connection, notice_url: str, content_hash: str) -> None:
    now = utc_now()
    conn.execute(
        """
        UPDATE notices
        SET sent_at=COALESCE(sent_at, ?),
            last_notified_hash=?
        WHERE link=?
        """,
        (now, content_hash, normalize_url(notice_url)),
    )
    conn.commit()


def mark_notification_attempt(conn: sqlite3.Connection, notice_url: str) -> None:
    conn.execute(
        "UPDATE notices SET notification_attempted_at=? WHERE link=?",
        (utc_now(), normalize_url(notice_url)),
    )


def seed_without_notification(conn: sqlite3.Connection, notice_url: str, content_hash: str) -> None:
    now = utc_now()
    conn.execute(
        """
        UPDATE notices
        SET sent_at=COALESCE(sent_at, ?),
            last_notified_hash=?
        WHERE link=?
        """,
        (now, content_hash, normalize_url(notice_url)),
    )


def truncate_text(text: str, limit: int) -> str:
    text = str(text).strip()
    if len(text) <= limit:
        return text
    return text[: limit - 3].rstrip() + "..."


def format_notice_message(notice: Notice, gh_run_no: str = "local", edited: bool = False) -> str:
    description = notice.body_text or notice.description or "Please click the link for details."
    gh_run_no = re.sub(r"[^A-Za-z0-9._-]", "", str(gh_run_no))[:64] or "local"
    header = (
        f"{'[EDITED] ' if edited else ''}{truncate_text(notice.title, 500)}\n\n"
        f"Date: {display_date(notice.published_date)}\n\n"
    )
    suffix = f"\n\n{notice.url}#{gh_run_no}"
    description = truncate_text(description, min(1600, max(4, 4096 - len(header) - len(suffix))))
    message = header + description + suffix
    if len(message) > 4096:
        raise ValueError(f"Telegram message exceeds 4096 characters for {notice.url}")
    return message


_last_telegram_send_at = 0.0
_telegram_delivery_blocked = False


class DetailFetchHealthError(RuntimeError):
    pass


def send_telegram_message(
    session: requests.Session,
    chat_id: str,
    message: str,
    config: dict[str, str | None],
    label: str,
) -> bool:
    global _telegram_delivery_blocked
    if _telegram_delivery_blocked:
        return False
    if DRY_RUN:
        preview = (
            message if len(message) <= DRY_RUN_PREVIEW_CHARS
            else message[:DRY_RUN_PREVIEW_CHARS] + "...[truncated]"
        )
        logging.info(
            "DRY_RUN: would send '%s' (%d chars)\n"
            "----- message preview -----\n%s\n----- end preview -----",
            label, len(message), preview,
        )
        return True

    bot_api_key = str(config["bot_api_key"])
    url = f"https://api.telegram.org/bot{bot_api_key}/sendMessage"
    payload = {
        "chat_id": chat_id,
        "text": message,
        "disable_web_page_preview": True,
    }

    global _last_telegram_send_at
    for attempt in range(TELEGRAM_MAX_RETRIES):
        pause = 1.0 - (time.monotonic() - _last_telegram_send_at)
        if pause > 0:
            time.sleep(pause)
        _last_telegram_send_at = time.monotonic()
        try:
            response = session.post(url, json=payload, timeout=REQUEST_TIMEOUT)
        except requests.RequestException as exc:
            logging.error("Telegram send failed for %s: %s", label, str(exc).replace(bot_api_key, "<redacted>"))
            return False  # A timeout may occur after Telegram accepted the message.

        try:
            result = response.json()
        except ValueError:
            result = {}
        if not isinstance(result, dict):
            result = {}
        if response.status_code == 200 and result.get("ok") is True:
            logging.info("Sent %s to Telegram.", label)
            return True
        if response.status_code == 429 or result.get("error_code") == 429:
            parameters = result.get("parameters") or {}
            retry_after = parameters.get("retry_after") if isinstance(parameters, dict) else None
            retry_after = retry_after or getattr(response, "headers", {}).get("Retry-After")
            try:
                delay = max(1.0, float(retry_after))
            except (TypeError, ValueError):
                delay = TELEGRAM_RETRY_BACKOFF * (attempt + 1)
            if delay > TELEGRAM_MAX_RETRY_DELAY:
                _telegram_delivery_blocked = True
                logging.error(
                    "Telegram requested a %.0f-second retry delay; deferring remaining messages.",
                    delay,
                )
                return False
            if attempt + 1 < TELEGRAM_MAX_RETRIES:
                time.sleep(delay)
                continue
            _telegram_delivery_blocked = True
        error = str(result.get("description") or f"HTTP {response.status_code}")
        logging.error("Telegram send failed for %s: %s", label, error.replace(bot_api_key, "<redacted>"))
        return False
    return False


def process_notices(
    conn: sqlite3.Connection,
    aiub_session: requests.Session,
    telegram_session: requests.Session,
    notices: list[Notice],
    config: dict[str, str | None],
    first_run: bool,
    existing_by_url: Mapping[str, sqlite3.Row] | None = None,
) -> tuple[int, int, int]:
    global _telegram_delivery_blocked
    _telegram_delivery_blocked = False
    new_count = 0
    edited_count = 0
    failed_notifications = 0
    unchanged_count = 0
    chat_id = str(config["chat_id"])
    gh_run_no = str(config.get("github_run_number") or "local")
    if existing_by_url is None:
        existing_by_url = load_existing_notice_rows(conn)

    listing_by_url = {notice.url: notice for notice in notices}
    new_candidates = [notice for notice in notices if notice.url not in existing_by_url]
    retry_cutoff = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(minutes=30)).replace(microsecond=0).isoformat()

    def detail_due(row: sqlite3.Row) -> bool:
        checked = row["detail_checked_at"]
        failed_last = checked and (not row["detail_fetched_at"] or checked > row["detail_fetched_at"])
        return not failed_last or checked <= retry_cutoff

    newest_targets = [] if first_run else [
        notice for notice in notices[:10]
        if notice.url not in existing_by_url or detail_due(existing_by_url[notice.url])
    ]
    selected_urls = {notice.url for notice in newest_targets}
    new_targets = [notice for notice in new_candidates if notice.url not in selected_urls][
        :DETAIL_FETCH_LIMIT if first_run else 10
    ]
    selected_urls.update(notice.url for notice in new_targets)
    pending_budget = max(0, 10 - len(new_targets)) if not first_run else 0
    eligible_pending = [
        row for url, row in existing_by_url.items()
        if row["detail_fetched_at"] is None and url not in selected_urls and detail_due(row)
    ]
    failed_details = sorted(
        (row for row in eligible_pending if row["detail_checked_at"]),
        key=lambda row: row["detail_checked_at"],
    )
    never_checked = sorted(
        (row for row in eligible_pending if not row["detail_checked_at"]),
        key=lambda row: row["first_seen_at"], reverse=True,
    )
    retry_slots = min(len(failed_details), (pending_budget + 1) // 2)
    pending_rows = failed_details[:retry_slots] + never_checked[:pending_budget - retry_slots]
    pending_rows += failed_details[retry_slots:retry_slots + pending_budget - len(pending_rows)]
    pending_targets = [
        listing_by_url.get(row["link"])
        or Notice(row["title"], row["description"], row["link"], row["published_date"])
        for row in pending_rows
    ]
    selected_urls.update(notice.url for notice in pending_targets)
    recent_targets = sorted(
        (notice for notice in notices[10:LIST_PAGE_SIZE]
         if notice.url in existing_by_url and notice.url not in selected_urls
         and detail_due(existing_by_url[notice.url])),
        key=lambda notice: existing_by_url[notice.url]["detail_checked_at"] or "",
    )[:3 if not first_run else 0]
    selected_urls.update(notice.url for notice in recent_targets)
    recent_urls = {notice.url for notice in notices[:LIST_PAGE_SIZE]}
    archive_rows = sorted(
        (row for url, row in existing_by_url.items()
         if url not in recent_urls and url not in selected_urls and detail_due(row)),
        key=lambda row: row["detail_checked_at"] or "",
    )[:2 if not first_run else 0]
    archive_targets = [
        listing_by_url.get(row["link"])
        or Notice(row["title"], row["description"], row["link"], row["published_date"])
        for row in archive_rows
    ]
    retry_rows = sorted(
        (row for url, row in existing_by_url.items()
         if url not in listing_by_url
         and (row["sent_at"] is None or row["last_notified_hash"] != row["content_hash"])),
        key=lambda row: (row["notification_attempted_at"] is not None,
                         row["notification_attempted_at"] or row["content_changed_at"]),
    )[:20]
    retry_targets = [
        Notice(row["title"], row["description"], row["link"], row["published_date"])
        for row in retry_rows
    ]
    priority_retries = retry_targets[:5]
    detail_limit = DETAIL_FETCH_LIMIT if first_run else 25
    detail_targets: list[Notice] = []
    detail_urls: set[str] = set()
    for target in (
        newest_targets + new_targets + pending_targets + priority_retries + recent_targets + archive_targets
    ):
        if target.url not in detail_urls and len(detail_targets) < detail_limit:
            detail_targets.append(target)
            detail_urls.add(target.url)
    logging.info("Fetching %s detail pages (%s new, %s pending, %s retries, %s newest, %s recent, %s archive).",
                 len(detail_targets), len(new_targets), len(pending_targets), len(priority_retries),
                 len(newest_targets), len(recent_targets), len(archive_targets))
    enriched_by_url: dict[str, Notice] = {}
    for i, notice in enumerate(detail_targets, 1):
        logging.info("Detail fetch [%s/%s]: %s", i, len(detail_targets), notice.url)
        enriched_by_url[notice.url] = enrich_notice(aiub_session, notice)

    failed_details_count = sum(not notice.detail_fetched for notice in enriched_by_url.values())
    if (len(enriched_by_url) >= DETAIL_FAILURE_MIN_SAMPLE
            and failed_details_count / len(enriched_by_url) >= DETAIL_FAILURE_RATIO):
        raise DetailFetchHealthError(
            f"{failed_details_count} of {len(enriched_by_url)} detail pages failed validation"
        )

    # Reserve five send slots for off-page retries while fresh listings are busy.
    processed_urls = set(listing_by_url)
    for target in priority_retries:
        if target.url not in processed_urls:
            processed_urls.add(target.url)
    notices = priority_retries + notices
    for target in pending_targets + archive_targets + retry_targets:
        if target.url not in processed_urls:
            notices.append(target)
            processed_urls.add(target.url)

    send_attempts = 0
    for listing_notice in notices:
        existing = existing_by_url.get(listing_notice.url)
        checked = listing_notice.url in enriched_by_url
        enriched = enriched_by_url.get(listing_notice.url)
        notice = enriched if enriched and enriched.detail_fetched else merge_cached_notice(listing_notice, existing)
        new_hash = notice_hash(notice)
        previous_hash = existing["content_hash"] if existing else None
        previous_notified_hash = existing["last_notified_hash"] if existing else None
        # Legacy cached titles may be the full detail title while listings abbreviate it.
        title_changed = bool(existing and enriched and enriched.detail_fetched
                             and enriched.title != existing["title"]
                             and (existing["detail_fetched_at"] is not None
                                  or not titles_equivalent(listing_notice.title, existing["title"])))
        listing_changed = bool(existing and (
            title_changed
            or listing_notice.description != existing["description"]
            or (listing_notice.published_date and listing_notice.published_date != existing["published_date"])
        ))
        legacy_detail_backfill = False
        if existing and enriched and enriched.detail_fetched and existing["detail_fetched_at"] is None:
            cached_body = clean_text(existing["body_text"] or "")
            ignored_legacy_links = {BASE_URL.rstrip("/"), notice.url.rstrip("/")}
            cached_attachments = {
                normalized
                for attachment in parse_attachments_json(existing["attachments_json"])
                if (normalized := normalize_url(attachment, allow_external=True))
                and normalized.rstrip("/") not in ignored_legacy_links
            }
            enriched_attachments = {
                normalized
                for attachment in enriched.attachments or []
                if (normalized := normalize_url(attachment, allow_external=True))
            }
            legacy_detail_backfill = (
                (not cached_body and not cached_attachments)
                or (
                    cached_body == clean_text(enriched.body_text)
                    and cached_attachments.issubset(enriched_attachments)
                    and titles_equivalent(enriched.title, existing["title"])
                )
            )
        parser_normalization_backfill = False
        if (existing and enriched and enriched.detail_fetched
                and enriched.legacy_body_text is not None
                and enriched.legacy_attachments is not None and not listing_changed):
            cached_attachments = set(parse_attachments_json(existing["attachments_json"]))
            parser_normalization_backfill = (
                clean_text(existing["body_text"] or "") == clean_text(enriched.legacy_body_text)
                and cached_attachments == set(enriched.legacy_attachments)
                and titles_equivalent(enriched.title, existing["title"])
            )
        backfill = bool(
            existing and previous_hash != new_hash and previous_notified_hash == previous_hash
            and enriched and enriched.detail_fetched
            and (legacy_detail_backfill or parser_normalization_backfill) and not listing_changed
        )

        upsert_seen_notice(conn, notice, new_hash, existing, checked,
                           historical=first_run, backfill=backfill)

        if existing is None:
            new_count += 1
            if first_run:
                seed_without_notification(conn, notice.url, new_hash)
                logging.info("[SEED-FIRSTRUN] %s -> %s", notice.title, notice.url)
                continue

            logging.info("[NEW] %s -> %s", notice.title, notice.url)
            if send_attempts >= TELEGRAM_SEND_LIMIT or _telegram_delivery_blocked:
                logging.info("[NEW-DEFERRED] %s -> %s", notice.title, notice.url)
                continue
            mark_notification_attempt(conn, notice.url)
            send_attempts += 1
            message = format_notice_message(notice, gh_run_no)
            if send_telegram_message(telegram_session, chat_id, message, config, notice.title):
                mark_notified(conn, notice.url, new_hash)
            else:
                failed_notifications += 1
                logging.warning("[NEW-SEND-FAILED] %s -> %s", notice.title, notice.url)
            continue

        if existing["sent_at"] is None:
            if send_attempts >= TELEGRAM_SEND_LIMIT or _telegram_delivery_blocked:
                logging.info("[RESEND-DEFERRED] %s -> %s", notice.title, notice.url)
                continue
            logging.info("[RESEND-UNSENT] %s -> %s", notice.title, notice.url)
            mark_notification_attempt(conn, notice.url)
            send_attempts += 1
            message = format_notice_message(notice, gh_run_no)
            if send_telegram_message(telegram_session, chat_id, message, config, notice.title):
                mark_notified(conn, notice.url, new_hash)
            else:
                failed_notifications += 1
                logging.warning("[RESEND-FAILED] %s -> %s", notice.title, notice.url)
            continue

        if previous_notified_hash == new_hash:
            unchanged_count += 1
            logging.debug("[UNCHANGED] %s -> %s", notice.title, notice.url)
            continue

        if backfill:
            seed_without_notification(conn, notice.url, new_hash)
            logging.info("[BACKFILL-NO-NOTIFY] %s -> %s", notice.title, notice.url)
            continue

        edited_count += 1
        if send_attempts >= TELEGRAM_SEND_LIMIT or _telegram_delivery_blocked:
            logging.info("[EDIT-DEFERRED] %s -> %s", notice.title, notice.url)
            continue
        logging.info("[EDITED] %s -> %s (notified_hash=%s new_hash=%s)",
                     notice.title, notice.url, (previous_notified_hash or "")[:8], new_hash[:8])
        mark_notification_attempt(conn, notice.url)
        send_attempts += 1
        message = format_notice_message(notice, gh_run_no, edited=True)
        if send_telegram_message(telegram_session, chat_id, message, config, notice.title):
            mark_notified(conn, notice.url, new_hash)
        else:
            failed_notifications += 1
            logging.warning("[EDIT-SEND-FAILED] %s -> %s", notice.title, notice.url)

    logging.info(
        "process_notices summary: new=%s edited=%s unchanged=%s failed=%s",
        new_count, edited_count, unchanged_count, failed_notifications,
    )
    conn.commit()
    return new_count, edited_count, failed_notifications


def generate_rss_feed(conn: sqlite3.Connection) -> None:
    rows = conn.execute(
        """
        SELECT title, description, link, published_date, body_text, content_changed_at
        FROM notices
        ORDER BY content_changed_at DESC, id ASC
        LIMIT ?
        """,
        (RSS_ITEM_LIMIT,),
    ).fetchall()
    logging.info("Generating RSS feed with %s items.", len(rows))

    etree.register_namespace("atom", "http://www.w3.org/2005/Atom")
    rss = etree.Element("rss", version="2.0")
    channel = etree.SubElement(rss, "channel")
    etree.SubElement(channel, "title").text = "AIUB Notices"
    etree.SubElement(channel, "link").text = f"{BASE_URL}{NOTICE_PATH}"
    etree.SubElement(channel, "description").text = "Latest notices from AIUB."
    last_build = conn.execute("SELECT value FROM app_state WHERE key='rss_last_build_at'").fetchone()
    last_build_at = last_build[0] if last_build else utc_now()
    build_date = etree.SubElement(channel, "lastBuildDate")
    build_date.text = format_datetime(
        dt.datetime.fromisoformat(last_build_at).astimezone(dt.timezone.utc), usegmt=True,
    )

    self_link = etree.SubElement(channel, "{http://www.w3.org/2005/Atom}link")
    self_link.set("rel", "self")
    self_link.set("type", "application/rss+xml")
    self_link.set("href", "https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/rss.xml")

    skipped = 0
    for row in rows:
        link = normalize_url(row["link"])
        if not link:
            logging.warning("Skipping RSS item with invalid link: %r", row["link"])
            skipped += 1
            continue
        item = etree.SubElement(channel, "item")
        etree.SubElement(item, "title").text = row["title"]
        description = truncate_text(row["body_text"] or row["description"], 2000)
        etree.SubElement(item, "description").text = description
        etree.SubElement(item, "link").text = link
        etree.SubElement(item, "guid").text = link
        etree.SubElement(item, "{http://www.w3.org/2005/Atom}updated").text = row["content_changed_at"]
        pub_date = rss_pub_date(row["published_date"])
        if pub_date:
            etree.SubElement(item, "pubDate").text = pub_date

    feed_bytes = etree.tostring(rss, encoding="UTF-8", xml_declaration=True)
    if os.path.exists(RSS_FEED_FILE):
        with open(RSS_FEED_FILE, "rb") as existing_feed:
            if existing_feed.read() == feed_bytes:
                logging.info("RSS feed unchanged.")
                return
    last_build_at = utc_now()
    build_date.text = format_datetime(
        dt.datetime.fromisoformat(last_build_at).astimezone(dt.timezone.utc), usegmt=True,
    )
    feed_bytes = etree.tostring(rss, encoding="UTF-8", xml_declaration=True)
    feed_dir = os.path.dirname(os.path.abspath(RSS_FEED_FILE)) or "."
    os.makedirs(feed_dir, exist_ok=True)
    temp_path = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            delete=False,
            dir=feed_dir,
            prefix=f".{os.path.basename(RSS_FEED_FILE)}.",
            suffix=".tmp",
        ) as temp_file:
            temp_path = temp_file.name
            temp_file.write(feed_bytes)
        os.replace(temp_path, RSS_FEED_FILE)
    finally:
        if temp_path and os.path.exists(temp_path):
            os.unlink(temp_path)
    conn.execute(
        "INSERT INTO app_state(key, value) VALUES('rss_last_build_at', ?) "
        "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (last_build_at,),
    )
    conn.commit()
    logging.info("RSS feed generated at %s (%s items, %s skipped).", RSS_FEED_FILE, len(rows) - skipped, skipped)


def rss_pub_date(iso_date: str | None) -> str | None:
    if not iso_date:
        return None
    try:
        date_value = dt.date.fromisoformat(iso_date)
        datetime_value = dt.datetime.combine(date_value, dt.time(), dt.timezone.utc)
        return format_datetime(datetime_value, usegmt=True)
    except ValueError:
        return None


def load_config() -> dict[str, str | None]:
    bot_api_key = os.environ.get("TELEGRAM_BOT_API_KEY")
    chat_id = os.environ.get("TELEGRAM_CHAT_ID")
    if not DRY_RUN and (not bot_api_key or not chat_id):
        raise RuntimeError("TELEGRAM_BOT_API_KEY and TELEGRAM_CHAT_ID are required unless DRY_RUN=true.")

    return {
        "bot_api_key": bot_api_key or "dry-run",
        "chat_id": chat_id or "dry-run",
        "admin_chat_id": os.environ.get("TELEGRAM_ADMIN_CHAT_ID"),
        "github_run_number": os.environ.get("GITHUB_RUN_NUMBER", "local"),
    }


def main() -> int:
    configure_logging()
    logging.info("AIUB notice checker v%s starting. DRY_RUN=%s", SCRIPT_VERSION, DRY_RUN)
    config = load_config()
    logging.info(
        "Config: Telegram destination configured; admin alerts configured=%s",
        bool(config["admin_chat_id"]),
    )
    aiub_session = create_aiub_session()
    telegram_session = create_telegram_session()

    with closing(connect_run_db()) as conn, conn:
        ensure_schema(conn)
        first_run = conn.execute("SELECT COUNT(*) FROM notices").fetchone()[0] == 0
        logging.info("first_run=%s", first_run)
        existing_by_url = load_existing_notice_rows(conn)
        known_links = set(existing_by_url)
        last_full_scan = conn.execute(
            "SELECT value FROM app_state WHERE key='last_full_scan_at'"
        ).fetchone()
        full_scan = first_run or not last_full_scan or last_full_scan[0][:10] != dt.datetime.now(dt.timezone.utc).date().isoformat()

        try:
            notices = crawl_notices(aiub_session, known_links, full_scan=full_scan)
        except (requests.RequestException, ValueError, etree.ParserError) as exc:
            logging.error("AIUB notices crawl failed: %s", exc)
            admin_chat_id = config.get("admin_chat_id")
            if admin_chat_id:
                send_telegram_message(
                    telegram_session,
                    str(admin_chat_id),
                    (
                        "AIUB Notice\n\nThe AIUB notices crawl failed. "
                        "This run will be retried by the next schedule."
                    ),
                    config,
                    "admin notification",
                )
            return 1

        if not notices:
            admin_chat_id = config.get("admin_chat_id")
            if admin_chat_id:
                send_telegram_message(
                    telegram_session,
                    str(admin_chat_id),
                    (
                        "AIUB Notice\n\nNo notices were found on the listing page. "
                        "The page structure or parser may need to be updated."
                    ),
                    config,
                    "admin notification",
                )
            raise RuntimeError("No notices were collected from AIUB.")

        logging.info("Collected %s unique notices from listing pages.", len(notices))
        try:
            new_count, edited_count, failed_notifications = process_notices(
                conn,
                aiub_session,
                telegram_session,
                notices,
                config,
                first_run,
                existing_by_url,
            )
        except DetailFetchHealthError as exc:
            logging.error("AIUB detail-page health check failed: %s", exc)
            admin_chat_id = config.get("admin_chat_id")
            if admin_chat_id:
                send_telegram_message(
                    telegram_session,
                    str(admin_chat_id),
                    "AIUB Notice\n\nMost sampled detail pages failed validation. The site parser may need an update.",
                    config,
                    "admin notification",
                )
            return 1
        if full_scan:
            conn.execute(
                "INSERT INTO app_state(key, value) VALUES('last_full_scan_at', ?) "
                "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                (utc_now(),),
            )
            conn.commit()
        if not DRY_RUN:
            generate_rss_feed(conn)

    logging.info(
        "Script completed. New=%s, Edited=%s, Failed notifications=%s",
        new_count,
        edited_count,
        failed_notifications,
    )
    return 1 if failed_notifications else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        logging.exception("Script failed")
        raise SystemExit(1) from None
