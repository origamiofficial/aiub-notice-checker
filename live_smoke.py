from __future__ import annotations

from urllib.parse import parse_qs, urljoin, urlparse

import main


def pagination_pages(tree) -> list[int]:
    return [
        int(value)
        for href in tree.xpath(main.PAGINATION_XPATH)
        if urlparse(urljoin(main.BASE_URL, href)).path == main.NOTICE_PATH
        for value in parse_qs(urlparse(href).query).get("pageNo", [])
        if value.isdecimal()
    ]


def run() -> None:
    session = main.create_aiub_session()
    first_url = f"{main.BASE_URL}{main.NOTICE_PATH}?pageNo=1&pageSize={main.LIST_PAGE_SIZE}"
    first_tree = main.fetch_html(session, first_url)
    pages = pagination_pages(first_tree)
    if not pages or 1 not in pages:
        raise RuntimeError("Live AIUB pagination is invalid")
    last_page = max(pages)
    page_numbers = sorted({1, (last_page + 1) // 2, last_page})
    checked_details: set[str] = set()

    for page_no in page_numbers:
        if page_no == 1:
            tree = first_tree
        else:
            url = f"{main.BASE_URL}{main.NOTICE_PATH}?pageNo={page_no}&pageSize={main.LIST_PAGE_SIZE}"
            tree = main.fetch_html(session, url)
        notices = main.parse_listing_page(tree)
        if not notices or (page_no < last_page and len(notices) != main.LIST_PAGE_SIZE):
            raise RuntimeError(f"Live AIUB archive page {page_no} is incomplete")
        for notice in (notices[0], notices[-1]):
            if notice.url in checked_details:
                continue
            detail = main.parse_detail_page(main.fetch_html(session, notice.url), notice)
            if not detail.detail_fetched:
                raise RuntimeError(f"Live AIUB detail validation failed: {notice.url}")
            checked_details.add(notice.url)

    print(
        f"Live smoke passed: archive_pages={last_page}, "
        f"listing_pages_checked={len(page_numbers)}, details_checked={len(checked_details)}"
    )


if __name__ == "__main__":
    run()
