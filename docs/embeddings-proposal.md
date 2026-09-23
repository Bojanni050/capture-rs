# Capture Embeddings — Architecture Proposal

## Status
Geïmplementeerd, **niet productie-klaar**. `embeddings.enabled` staat
standaard op `false`; zelfs aangezet blijft `provider = "mock"` de default,
dus een `enabled = true` alleen triggert nooit onverwacht een modeldownload.
Zet daarnaast `provider = "fastembed"` voor een echt lokaal model
(`intfloat/multilingual-e5-small`, via `FastEmbedProvider` in
`embeddings/provider.rs`, ~118 MB eenmalige download, ONNX Runtime CPU). Zie
[§9 Bekende beperkingen](#9-bekende-beperkingen) voor wat nog ontbreekt
rondom de omliggende infrastructuur (gedeelde state, connection pooling).

## 1. Inspectie

- **pipeline.rs** : capture → `filter.gate` → `dhash` → `read_text (UIA→OCR)` → `analyze_text` → `insert_capture` (SQLite) → return. SQLite is synchronous via `Mutex<Connection>`. Geen vector logica. Insertion point = direct na `insert_capture`.
- **store/db.rs** : SQLite + FTS5 `captures_fts`, `segments`, `apps`, `captures`. Purge via `captures_before`/`frames_before`. Geen vector tabel. `Db` is `Mutex<Connection>` — blocking.
- **filter/** : `gate` (idle/denylist), `dedupe` (dHash), `text` (normalize/quality/jaccard), `privacy` (redaction). Redactie gebeurt vóór `index_text` → embedding moet `index_text` gebruiken (al geredigeerd).
- **config.rs** : TOML met `validate()`. Sections `capture/uia/ocr/filter/storage/server`. Embeddings nieuw section consistent.
- **server/mod.rs** : `axum`, handlers nu via `spawn_blocking`. Routes `search/timeline/stats/apps/capture/frame`.
- **PostgreSQL** : `postgresql-x64-18` running op 5432, DB `postgres` met password `postgres` (getest `127.0.0.1`), `psql` onder `C:\Program Files\PostgreSQL\18\bin`. **Geen `pgvector` extensie** geïnstalleerd — `pg_available_extensions` toont 0 vector rows, `share/extension` bevat geen `vector.control`. Installatie vereist: download `pgvector` Windows binaries voor PG18 of bouw van source.

## 2. Model keuze (lokaal, Windows, Rust, NL+EN)

- **Aanrader:** `fastembed` (Rust, ONNX via `ort`) met `intfloat/multilingual-e5-small`
  - 118 MB, 384 dims, quantized ONNX, ~30 ms / doc op CPU, NL+EN goed (MTEB multilingual).
  - Alternatief klein: `paraphrase-multilingual-MiniLM-L12-v2` 384 dims, 471 MB, ouder.
  - Groter: `intfloat/multilingual-e5-base` 278 MB, 768 dims, +2-3% recall, dubbele RAM/latency — niet nodig voor v1.
  - Te zwaar: `BAAI/bge-m3` 1024 dims, >2 GB, ongeschikt voor continuous desktop.
- Tradeoff: `e5-small` past in 150 MB RAM, start <1s, downloads bij eerste run via `hf-hub` cache `%LOCALAPPDATA%/huggingface`. Niet bundelen.
- Provider trait geabstraheerd zodat model later vervangbaar is.

## 3. Voorgestelde architectuur (kleinste robuuste stap)

```
UIA/OCR → normalize → filter.gate/dedupe/text/privacy → SQLite/FTS5 (truth)
                                                      └─► mpsc::channel(capture_id) ─► Indexer task
                                                                                           │
                                                                     batch + dedup (content_hash)
                                                                                           ▼
                                                                              EmbeddingProvider::embed_batch
                                                                                           ▼
                                                                              PostgreSQL + pgvector (optioneel)
```

- **SQLite blijft truth** — capture retourneert direct na `insert_capture`, daarna `try_send` naar channel (bounded 1024, drop bij vol → backpressure log, nooit block).
- **Semantic unit** ≠ 1 capture. Voor v1: **segment-window** (zelfde `app_key` + `title` binnen 2–5 min, of `segment` zelf). Bouw `content = app + title + concatenated index_text (≤800 tokens)` deterministisch, hash via `xxhash` van `index_text` om duplicaten te skippen. Bewaar `source_capture_ids: Vec<i64>` als provenance.
- **Async** : `tokio::spawn` indexer die `spawn_blocking` voor embedding + `sqlx`/`tokio-postgres` voor pgvector. Bij PG down: retry met backoff, queue blijft, capture ongestoord. Bij embedding fail: log, source nooit verloren.
- **Pgvector schema** (wanneer beschikbaar):
  ```sql
  CREATE EXTENSION IF NOT EXISTS vector;
  CREATE TABLE semantic_documents (
    id UUID PRIMARY KEY,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL UNIQUE,
    application TEXT NOT NULL,
    title TEXT NOT NULL,
    start_time BIGINT NOT NULL,
    end_time BIGINT NOT NULL,
    source_capture_ids BIGINT[] NOT NULL,
    source TEXT NOT NULL, -- uia/ocr/mixed
    has_frame BOOL NOT NULL,
    embedding VECTOR(384) NOT NULL,
    embedding_model TEXT NOT NULL,
    embedding_dimensions INT NOT NULL,
    created_at BIGINT NOT NULL
  );
  CREATE INDEX ON semantic_documents USING ivfflat (embedding vector_cosine_ops) WITH (lists=100);
  CREATE INDEX ON semantic_documents(application, start_time);
  ```
  Normalisatie: `source_capture_ids` als array volstaat voor v1; alternatief join-tabel `semantic_sources(semantic_id, capture_id)`.
- **Fallback** : als `vector` niet geïnstalleerd → embeddings `enabled=false` log warning, indexer slaat over, SQLite/FTS5 blijft werken. Tests zonder PG via `MockProvider` + `InMemoryStore`.

## 4. Config (volgt bestaande stijl)

```toml
[embeddings]
enabled = false
provider = "fastembed"   # of "mock" voor tests
model = "intfloat/multilingual-e5-small"
postgres_url = "postgres://postgres:postgres@127.0.0.1:5432/capture"
dimensions = 384
batch_size = 32
poll_interval_secs = 5.0
max_content_chars = 4000
```

Env override: `CAPTURE_POSTGRES_URL` (legacy: `CHRONICLE_POSTGRES_URL`) > `postgres_url`. Secrets nooit in TOML log.

## 5. Zoeken

- Behoud `GET /api/search?q=&from=&to=&limit=` voor FTS5.
- Nieuw: `GET /api/search/semantic?q=&limit=` of `GET /api/search?q=...&semantic=true` — kiest lexicaal vs. vector. Voor v1 aparte endpoint, later hybrid: `lexical + cosine*0.7 + time_decay`.
- CLI: `capture search "kwartaalrapport" --semantic "waar was ik met gaia proxy debugging"` — voegt `--semantic` flag toe naast bestaande `search`.

## 6. Provenance & retention

- Elk semantic doc traceerbaar: `capture(id) → segment → app/title/ts/source/has_frame`. API retourneert `source_capture_ids` + `content` + afstand.
- Purge: `DELETE FROM semantic_documents WHERE end_time < cutoff` in zelfde transactie als `Db::purge`. Rebuild: `capture embeddings rebuild` leest SQLite eligible rows, reconstrueert docs, regenereert embeddings (model versie in row laat mix detecteren).

## 7. Implementatievolgorde (kleinste stap eerst)

1. `embeddings` module + `EmbeddingProvider` trait + `MockProvider` + `FastEmbedProvider` (optional feature).
2. Config + `store/pgvector` (feature-gated, graceful degrade als geen PG/vector).
3. `document.rs` deterministic builder + hash dedup.
4. `indexer.rs` async queue gehaakt na `insert_capture`.
5. HTTP + CLI semantic search.
6. `embeddings status/rebuild/purge` CLI + purge hook.

## 8. Open punten

- `pgvector` installeren op Windows PG18 (handmatig via release zip `pgvector-0.8.0-pg18-windows`). Zonder is semantic index "pending".
- Model download bij eerste run — documenteer 120 MB.

## 9. Bekende beperkingen

De implementatie (`src/embeddings/`) volgt dit voorstel structureel, maar is op
een aantal punten blijven steken bij scaffolding. Twee correctheidsbugs zijn
gefixt (zie git-historie: `fetch_captures_since` gebruikte `captures.text` in
plaats van de gefilterde tekst uit `captures_fts`, en de indexer-cursor
overleefde geen herstart). De rest staat hier gedocumenteerd, bewust niet
opgelost:

- ~~Geen echt embeddingmodel~~ — **opgelost.** `FastEmbedProvider`
  (`embeddings/provider.rs`) gebruikt `intfloat/multilingual-e5-small` via
  `fastembed`/ONNX Runtime, met de query/passage-voorvoegsels die dit
  specifieke model verwacht (`embed_query` vs `embed` op de
  `EmbeddingProvider`-trait). `make_provider` is nu `async` en laadt het
  model via `spawn_blocking` (de download bij een lege cache mag nooit de
  tokio-runtime blokkeren); mislukt dat laden — geen internet bij de eerste
  run, ONNX-probleem — dan valt het terug op `MockProvider` met een
  zichtbare `WARN`, niet stil. Cache leeft in `<datamap>/models`, niet in
  `fastembed`'s eigen werkmap-relatieve default. Bewezen met een live test
  (`fastembed_herkent_verwante_tekst_niet_alleen_letterlijke_match`,
  `#[ignore]`d — downloadt echt het model): twee formuleringen van hetzelfde
  idee scoren aantoonbaar hoger dan een ongerelateerd paar, in tegenstelling
  tot de mock. `MockProvider` blijft de default (`provider = "mock"`) zodat
  `enabled = true` nooit ongevraagd een download triggert — `fastembed` moet
  je expliciet aanzetten.
- **`InMemoryStore` is niet gedeeld tussen processen.** Zonder `postgres_url`
  bouwt elke processtart zijn eigen lege `Vec` op. `cmd_start` deelt één
  instantie tussen zijn eigen indexer en webserver (vandaar dat de HTTP-API
  wél werkt), maar een losse CLI-aanroep (`capture search --semantic`,
  `capture embeddings status/rebuild`) krijgt altijd een verse, lege store.
  `rebuild` meldt dan "N documenten geïndexeerd" voor werk dat bij het
  afsluiten van het proces alweer weg is — een foutmelding zou hier eerlijker
  zijn dan een geslaagd ogende no-op. Met de cursor nu wél persistent (zie
  boven) geldt dit ook na een herstart: captures die ooit "verwerkt" zijn
  volgens de cursor, maar nooit in een duurzame store terechtkwamen, worden
  niet opnieuw geprobeerd. Alleen `capture embeddings rebuild` haalt ze dan
  nog terug.
- **Geen connection pooling.** Elke `VectorStore`-aanroep
  (`ensure_schema`/`upsert`/`search`/`delete_before`/`count`) opent een eigen
  `tokio_postgres::connect`. Werkt, maar een verbinding per aanroep in plaats
  van een pool is nodeloos duur zodra dit vaker draait dan eens per paar
  seconden.
- **`InMemoryStore` gebruikt kale `.unwrap()`** op zijn `RwLock`
  (`store.rs`), inconsistent met de rest van de codebase — overal elders
  (`store/db.rs`, `uia/`) herstelt een vergiftigd lock zich via
  `unwrap_or_else(|e| e.into_inner())` in plaats van te pankieken.
- **De directe notificatie-route is dode code.** `IndexCommand::Capture` en
  `make_indexer` (met zijn `mpsc`-kanaal voor "capture net binnen") worden
  nergens aangeroepen — `cmd_start` bouwt de indexer rechtstreeks op met
  `rx: None`. De indexer werkt uitsluitend via polling.
- **Niet genoemd in `README.md`.** Wie het naslaat zonder in `docs/` of
  `capture.toml` te kijken, weet niet dat dit bestaat.
