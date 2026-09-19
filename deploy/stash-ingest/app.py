"""
Minimal ingest service for CodexCapture's raw staging buffer ("Stash").

Accepts NDJSON batches at POST /ingest -- the exact contract shipper.py
already speaks -- and appends them to a local SQLite file. Deliberately
dumb: no embeddings, no LLM calls, no curation. A later selection pass
reads this store and decides what's worth promoting into Hindsight via
`retain`.
"""
import json
import os
import sqlite3
import time
from contextlib import closing

from flask import Flask, abort, jsonify, request

DB_PATH = os.environ.get("INGEST_DB_PATH", "/data/stash.sqlite3")
AUTH_TOKEN = os.environ.get("INGEST_TOKEN")  # optional shared secret

app = Flask(__name__)

_SCHEMA = """
CREATE TABLE IF NOT EXISTS captures (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    app TEXT NOT NULL,
    window_title TEXT NOT NULL,
    text TEXT NOT NULL,
    received_at REAL NOT NULL,
    UNIQUE(ts, app, window_title)
);
CREATE INDEX IF NOT EXISTS idx_captures_received_at ON captures(received_at);
"""


def _connect() -> sqlite3.Connection:
    os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)
    return sqlite3.connect(DB_PATH)


def _ensure_db() -> None:
    with closing(_connect()) as conn:
        conn.executescript(_SCHEMA)
        conn.commit()


_ensure_db()


def _check_auth() -> None:
    if not AUTH_TOKEN:
        return
    if request.headers.get("Authorization") != f"Bearer {AUTH_TOKEN}":
        abort(401)


@app.post("/ingest")
def ingest():
    _check_auth()
    body = request.get_data(as_text=True)
    inserted = 0
    skipped = 0
    with closing(_connect()) as conn:
        for line in body.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
                ts = float(record["ts"])
                app_name = str(record["app"])
                window_title = str(record["window_title"])
                text = str(record["text"])
            except (json.JSONDecodeError, KeyError, TypeError, ValueError):
                skipped += 1
                continue
            cur = conn.execute(
                "INSERT OR IGNORE INTO captures (ts, app, window_title, text, received_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (ts, app_name, window_title, text, time.time()),
            )
            if cur.rowcount:
                inserted += 1
            else:
                skipped += 1  # duplicate of an already-stored (ts, app, window_title)
        conn.commit()
    return jsonify({"inserted": inserted, "skipped": skipped}), 200


@app.get("/captures")
def list_captures():
    """Paginated, filterable read access for the local timeline GUI.
    Newest first; page with before_id (return rows with id < before_id)."""
    _check_auth()
    try:
        limit = min(max(int(request.args.get("limit", 50)), 1), 200)
    except ValueError:
        limit = 50

    clauses = []
    params: list = []

    before_id = request.args.get("before_id", "")
    if before_id:
        try:
            clauses.append("id < ?")
            params.append(int(before_id))
        except ValueError:
            pass

    app_filter = request.args.get("app", "").strip()
    if app_filter:
        clauses.append("app = ?")
        params.append(app_filter)

    q = request.args.get("q", "").strip()
    if q:
        clauses.append("(text LIKE ? OR window_title LIKE ?)")
        like = f"%{q}%"
        params.extend([like, like])

    where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
    with closing(_connect()) as conn:
        rows = conn.execute(
            f"SELECT id, ts, app, window_title, text, received_at FROM captures {where} "
            f"ORDER BY id DESC LIMIT ?",
            (*params, limit),
        ).fetchall()

    items = [
        {"id": r[0], "ts": r[1], "app": r[2], "window_title": r[3], "text": r[4], "received_at": r[5]}
        for r in rows
    ]
    return jsonify({"items": items}), 200


@app.get("/captures/apps")
def list_apps():
    """Distinct app names, for a filter dropdown in the timeline GUI."""
    _check_auth()
    with closing(_connect()) as conn:
        rows = conn.execute("SELECT DISTINCT app FROM captures ORDER BY app").fetchall()
    return jsonify({"apps": [r[0] for r in rows]}), 200


@app.get("/health")
def health():
    return jsonify({"status": "ok"}), 200


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=int(os.environ.get("PORT", 8080)))
