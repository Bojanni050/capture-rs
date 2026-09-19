# stash-ingest

Minimal raw staging buffer for CodexCapture. Accepts NDJSON batches at
`POST /ingest` — exactly the contract `shipper.py` already speaks — and
appends them to a local SQLite file. No embeddings, no LLM calls, no
curation: that happens later, in a separate selection pass that reads this
store and promotes what's worth keeping into Hindsight via `retain`.

## Deploy on the VPS

```bash
ssh jouw-gebruiker@je-vps
git clone <this-repo-or-copy-this-folder> stash-ingest
cd stash-ingest
docker compose up -d --build
docker compose logs -f   # check it started cleanly
```

Listens on port 8080 — same port `config.py`'s `STASH_ENDPOINT` already
expects, reachable over your existing Tailscale link
(`http://100.64.144.93:8080/ingest`). No public exposure or Plesk vhost
needed since Tailscale is IP-based.

## Optional auth

Uncomment `INGEST_TOKEN` in `docker-compose.yml` to require a shared
secret. If you do, set the same value as `STASH_AUTH_TOKEN` in the
CodexCapture agent's environment (`config.py` picks it up automatically
and sends it as `Authorization: Bearer <token>`).

## Data

SQLite file at `/data/stash.sqlite3` inside the container, persisted via
the `stash-data` named volume. Table `captures(id, ts, app, window_title,
text, received_at)`, with a `UNIQUE(ts, app, window_title)` constraint so a
retried shipment (e.g. after a network blip) doesn't create duplicates.

## Endpoints

- `POST /ingest` — body is NDJSON (one JSON object per line: `{"ts", "app", "window_title", "text"}`). Returns `{"inserted": N, "skipped": N}`.
- `GET /health` — liveness check, returns `{"status": "ok"}`.
