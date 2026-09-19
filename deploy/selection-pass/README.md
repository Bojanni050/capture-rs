# selection-pass

The missing link between raw capture and curated memory. Runs on a timer
(`RUN_INTERVAL_SEC`, default 30 min), reads new rows from `stash-ingest`'s
SQLite store, groups consecutive captures of the same window into
episodes, drops obvious noise, runs an LLM relevance pass, and calls
Hindsight's `POST /v1/default/banks/{bank_id}/memories` (`retain`, async)
with the result. Hindsight's own consolidation pipeline does the actual
fact-extraction from there — this script's job is *what gets sent at
all*, not how it's understood.

Two filtering layers before anything reaches Hindsight:
1. **Mechanical** — drop noise apps (`NOISE_APPS`) and episodes shorter than `MIN_CONTENT_CHARS`
2. **LLM relevance pass** (`RELEVANCE_FILTER_ENABLED`, default on) — an LLM call per batch (`RELEVANCE_BATCH_SIZE` items at a time) decides which episodes are worth remembering long-term versus routine/repetitive noise with no lasting value. Fails open on any API error — a bad filter pass never blocks or loses data, it just lets that batch through unfiltered.

Cursor-based (`selection.sqlite3` in its own volume): once a row's id has
been passed, it's never reprocessed, even if it was skipped as noise.

## Deploy on the VPS

Needs `stash-ingest` already deployed first (this reads its SQLite volume
directly, read-only).

```bash
ssh jouw-gebruiker@je-vps
cd /opt/selection-pass   # copy this folder here first
cp .env.example .env
nano .env                # confirm TAILSCALE_IP / HINDSIGHT_BANK_ID / RUN_INTERVAL_SEC
docker compose up -d --build
docker compose logs -f
```

No exposed ports — this is a pure background worker, nothing to bind or
firewall.

## Tuning

- `MIN_CONTENT_CHARS` (default 40) — episodes shorter than this are dropped as noise
- `NOISE_APPS` (default `explorer.exe,taskmgr.exe,searchhost.exe,shellexperiencehost.exe`) — comma-separated exe names, always skipped
- `EPISODE_GAP_SEC` (default 900 = 15 min) — captures of the same window further apart than this start a new episode instead of merging into one
- `RELEVANCE_FILTER_ENABLED` (default true), `LLM_API_KEY`, `LLM_BASE_URL` (default OpenRouter), `LLM_MODEL`, `RELEVANCE_BATCH_SIZE` (default 40) — the LLM relevance pass; set `RELEVANCE_FILTER_ENABLED=false` to fall back to the mechanical filters only
