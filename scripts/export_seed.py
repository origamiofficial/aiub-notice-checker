#!/usr/bin/env python3
"""Export existing rows from aiub_notices.db into D1-compatible SQL batches."""

from __future__ import annotations

import argparse
import sqlite3
from pathlib import Path

COLUMNS = (
    "title",
    "description",
    "link",
    "published_date",
    "body_text",
    "attachments_json",
    "content_hash",
    "first_seen_at",
    "last_seen_at",
    "sent_at",
    "last_notified_hash",
    "detail_checked_at",
    "detail_fetched_at",
    "notification_attempted_at",
    "content_changed_at",
)


def sql_literal(value: str | None) -> str:
    if value is None:
        return "NULL"
    escaped = str(value).replace("'", "''")
    return f"'{escaped}'"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", default="aiub_notices.db", help="Path to source SQLite DB")
    parser.add_argument("--out-dir", default="seed_batches", help="Output directory for SQL files")
    parser.add_argument("--batch-size", type=int, default=25, help="Rows per INSERT statement")
    parser.add_argument("--max-bytes", type=int, default=75_000, help="Max bytes per SQL file")
    args = parser.parse_args()

    db_path = Path(args.db)
    if not db_path.exists():
        raise SystemExit(f"Source database not found: {db_path}")

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(f"file:{db_path.resolve()}?mode=ro", uri=True)
    try:
        rows = conn.execute(f"SELECT {', '.join(COLUMNS)} FROM notices ORDER BY id ASC").fetchall()
    finally:
        conn.close()

    col_list = ", ".join(COLUMNS)
    current_lines: list[str] = []
    current_bytes = 0
    file_index = 1

    def flush() -> None:
        nonlocal current_lines, current_bytes, file_index
        if not current_lines:
            return
        target = out_dir / f"seed_{file_index:04d}.sql"
        target.write_text("\n".join(current_lines) + "\n", encoding="utf-8")
        file_index += 1
        current_lines = []
        current_bytes = 0

    for i in range(0, len(rows), args.batch_size):
        chunk = rows[i : i + args.batch_size]
        values_sql = ",\n  ".join(
            "(" + ", ".join(sql_literal(cell) for cell in row) + ")"
            for row in chunk
        )
        stmt = (
            f"INSERT INTO notices ({col_list}) VALUES\n  {values_sql}\n"
            "ON CONFLICT(link) DO NOTHING;"
        )
        stmt_bytes = len(stmt.encode("utf-8")) + 1
        if current_lines and current_bytes + stmt_bytes > args.max_bytes:
            flush()
        current_lines.append(stmt)
        current_bytes += stmt_bytes

    flush()

    marker = out_dir / f"seed_{file_index:04d}.sql"
    marker.write_text(
        "INSERT INTO app_state (key, value) VALUES ('seeded_from_sqlite', '1') "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value;\n",
        encoding="utf-8",
    )
    print(f"Exported {len(rows)} rows into {file_index} SQL batch file(s) in {out_dir}")


if __name__ == "__main__":
    main()