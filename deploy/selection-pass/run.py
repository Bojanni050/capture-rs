"""
Reads new raw captures from stash-ingest's SQLite store, groups them into
coherent episodes, filters obvious noise, and retains the result into
Hindsight. This is the piece that turns "everything captured" into
"curated memory" -- Hindsight's own consolidation pipeline (triggered on
retain) does the actual fact-extraction; this script's job is just:
skip noise, group into sensible documents, and hand off.

Cursor-based: never reprocesses a row once its id has been passed, even if
retaining it was skipped for being noise.
"""
import json
import os
import sqlite3
import time
from contextlib import closing

import requests

STASH_DB_PATH = os.environ.get("STASH_DB_PATH", "/data/stash.sqlite3")
STATE_DB_PATH = os.environ.get("STATE_DB_PATH", "/state/selection.sqlite3")
HINDSIGHT_URL = os.environ.get("HINDSIGHT_URL", "http://100.64.144.93:8888")
BANK_ID = os.environ.get("HINDSIGHT_BANK_ID", "bojan")
RUN_INTERVAL_SEC = float(os.environ.get("RUN_INTERVAL_SEC", "1800"))  # 30 min
MIN_CONTENT_CHARS = int(os.environ.get("MIN_CONTENT_CHARS", "40"))

# LLM relevance filter: a second, content-aware pass on top of the
# mechanical noise/length filters below, so routine/repetitive activity
# with no lasting value doesn't reach the permanent memory bank at all.
RELEVANCE_FILTER_ENABLED = os.environ.get("RELEVANCE_FILTER_ENABLED", "true").lower() == "true"
LLM_API_KEY = os.environ.get("LLM_API_KEY", "")
LLM_BASE_URL = os.environ.get("LLM_BASE_URL", "https://openrouter.ai/api/v1")
LLM_MODEL = os.environ.get("LLM_MODEL", "deepseek/deepseek-v4-flash")
RELEVANCE_BATCH_SIZE = int(os.environ.get("RELEVANCE_BATCH_SIZE", "40"))
NOISE_APPS = {
    a.strip().lower()
    for a in os.environ.get(
        "NOISE_APPS",
        "explorer.exe,taskmgr.exe,searchhost.exe,shellexperiencehost.exe,shellhost.exe,"
        "dwm.exe,lockapp.exe,credentialuibroker.exe,textinputhost.exe,widgetboard.exe,"
        "pickerhost.exe,phoneexperiencehost.exe,nearby_share.exe,crossdevicestreaminghost.exe,"
        "rundll32.exe,mmc.exe,powertoys.exe,powertoys.fancyzones.exe,powertoys.colorpickerui.exe",
    ).split(",")
    if a.strip()
}
# Consecutive captures of the same window past this gap start a new
# episode -- otherwise a monitor left on the same doc all day becomes one
# giant "episode".
EPISODE_GAP_SEC = float(os.environ.get("EPISODE_GAP_SEC", "900"))  # 15 min


def _stash_conn() -> sqlite3.Connection:
    return sqlite3.connect(STASH_DB_PATH)


def _state_conn() -> sqlite3.Connection:
    os.makedirs(os.path.dirname(STATE_DB_PATH), exist_ok=True)
    conn = sqlite3.connect(STATE_DB_PATH)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cursor (id INTEGER PRIMARY KEY CHECK (id = 1), last_row_id INTEGER NOT NULL)"
    )
    conn.execute("INSERT OR IGNORE INTO cursor (id, last_row_id) VALUES (1, 0)")
    conn.commit()
    return conn


def _get_cursor(state_conn: sqlite3.Connection) -> int:
    return state_conn.execute("SELECT last_row_id FROM cursor WHERE id = 1").fetchone()[0]


def _set_cursor(state_conn: sqlite3.Connection, row_id: int) -> None:
    state_conn.execute("UPDATE cursor SET last_row_id = ? WHERE id = 1", (row_id,))
    state_conn.commit()


def _fetch_new_rows(stash_conn: sqlite3.Connection, after_id: int):
    return stash_conn.execute(
        "SELECT id, ts, app, window_title, text FROM captures WHERE id > ? ORDER BY id ASC",
        (after_id,),
    ).fetchall()


def _group_into_episodes(rows):
    episodes = []
    current = None
    for row_id, ts, app, window_title, text in rows:
        if (
            current is not None
            and current["app"] == app
            and current["window_title"] == window_title
            and ts - current["last_ts"] <= EPISODE_GAP_SEC
        ):
            current["texts"].append(text)
            current["last_ts"] = ts
        else:
            if current is not None:
                episodes.append(current)
            current = {
                "app": app,
                "window_title": window_title,
                "start_ts": ts,
                "last_ts": ts,
                "start_row_id": row_id,
                "texts": [text],
            }
    if current is not None:
        episodes.append(current)
    return episodes


def _episode_to_memory_item(episode: dict):
    # buffer.py only ever wrote a new line when the text grew/changed
    # meaningfully, so the longest capture in the episode is usually its
    # most complete snapshot.
    content = max(episode["texts"], key=len).strip()
    if len(content) < MIN_CONTENT_CHARS:
        return None
    return {
        "content": content,
        "context": episode["app"],
        # start_row_id (stash-ingest's autoincrement PK) guarantees
        # uniqueness -- int(start_ts) alone can collide when two separate
        # episodes happen to start within the same rounded second (e.g. a
        # system dialog that grabs focus twice in quick succession).
        "document_id": f"{episode['app']}:{episode['window_title']}:{episode['start_row_id']}",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(episode["start_ts"])),
        "tags": [episode["app"]],
        "metadata": {"window_title": episode["window_title"]},
    }


def _filter_relevant_chunk(items: list) -> list:
    numbered = [
        {
            "index": i,
            "app": it["context"],
            "window_title": it["metadata"].get("window_title", ""),
            "content": it["content"][:500],
        }
        for i, it in enumerate(items)
    ]
    prompt = (
        "You are filtering a personal activity log before it's permanently stored as "
        "long-term memory. For each numbered item below, decide if it's worth "
        "remembering long-term (meaningful work, decisions, conversations, information "
        "learned, tasks, plans) versus trivial/repetitive noise with no lasting value.\n\n"
        "Treat as noise, always: routine browsing, media player controls, file "
        "listings, near-duplicate captures of the same activity, window chrome text "
        "like menus/buttons, and -- important -- any snapshot of transient OS/system "
        "state: volume level, system clock/time display, keyboard layout, taskbar "
        "pinned-app layout, notification counts, Quick Settings / Widgets panel "
        "contents, network/VPN/Tailscale connectivity status, battery/backup status. "
        "These describe the computer's state at a moment, not something the user did "
        "or decided -- they have no lasting value even though they sound factual.\n\n"
        "Reply with ONLY a JSON array of the indices worth keeping, nothing else. "
        "Example: [0, 3, 7]\n\n"
        f"Items:\n{json.dumps(numbered, ensure_ascii=False)}"
    )
    resp = requests.post(
        f"{LLM_BASE_URL}/chat/completions",
        headers={"Authorization": f"Bearer {LLM_API_KEY}", "Content-Type": "application/json"},
        json={"model": LLM_MODEL, "messages": [{"role": "user", "content": prompt}], "temperature": 0},
        timeout=60,
    )
    resp.raise_for_status()
    text = resp.json()["choices"][0]["message"]["content"].strip()
    start, end = text.find("["), text.rfind("]")
    if start == -1 or end == -1:
        raise ValueError(f"no JSON array found in LLM response: {text[:200]!r}")
    keep_indices = set(json.loads(text[start : end + 1]))
    return [it for i, it in enumerate(items) if i in keep_indices]


def _filter_relevant(items: list) -> list:
    """Fails open (keeps the chunk as-is) on any error -- a bad filter
    pass should never be the reason real data gets lost, only the reason
    some noise slips through."""
    if not RELEVANCE_FILTER_ENABLED or not LLM_API_KEY or not items:
        return items

    kept = []
    for i in range(0, len(items), RELEVANCE_BATCH_SIZE):
        chunk = items[i : i + RELEVANCE_BATCH_SIZE]
        try:
            kept.extend(_filter_relevant_chunk(chunk))
        except Exception as e:
            print(f"[selection-pass] relevance filter failed ({e}) -- keeping this chunk as-is")
            kept.extend(chunk)
    return kept


def _retain_batch(items: list) -> None:
    url = f"{HINDSIGHT_URL}/v1/default/banks/{BANK_ID}/memories"
    resp = requests.post(url, json={"items": items, "async": True}, timeout=30)
    resp.raise_for_status()


def run_once() -> int:
    """Returns the number of memory items retained this cycle."""
    with closing(_stash_conn()) as stash_conn, closing(_state_conn()) as state_conn:
        cursor = _get_cursor(state_conn)
        rows = _fetch_new_rows(stash_conn, cursor)
        if not rows:
            return 0

        max_id = max(row[0] for row in rows)
        kept_rows = [row for row in rows if row[2].lower() not in NOISE_APPS]
        episodes = _group_into_episodes(kept_rows)

        items = []
        for episode in episodes:
            item = _episode_to_memory_item(episode)
            if item is not None:
                items.append(item)

        items = _filter_relevant(items)

        if items:
            # Not caught here on purpose: if this raises, the cursor below
            # never advances, so these rows get retried next cycle instead
            # of silently skipped.
            _retain_batch(items)

        _set_cursor(state_conn, max_id)
        return len(items)


def main() -> None:
    print(f"[selection-pass] starting, bank={BANK_ID}, interval={RUN_INTERVAL_SEC}s")
    while True:
        try:
            n = run_once()
            if n:
                print(f"[selection-pass] retained {n} memory item(s)")
        except Exception as e:
            print(f"[selection-pass] error: {e} -- will retry next cycle")
        time.sleep(RUN_INTERVAL_SEC)


if __name__ == "__main__":
    main()
