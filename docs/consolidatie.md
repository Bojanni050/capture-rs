# Consolidatievoorstel: chronicle-rs & chroniclecapture

> Status: voorstel ter beslissing · Aangemaakt: 2026-09-18

## Doel

Eén activiteitenopname-oplossing ("wat gebeurt er op de computer") in plaats van twee parallelle repo's.

## Wat er nu is

### chronicle-rs (Rust, Windows)

Lokale activiteitenopname als native binary, volledig zelf gebouwd:

- `src/capture`, `src/uia`, `src/ocr` — schermtekst via Windows UI Automation, OCR als fallback
- `src/filter` — ruisfilter in vier lagen
- `src/pipeline` — verwerkingspijplijn
- `src/store` — eigen opslag
- `src/server` — eigen API-laag
- `src/embeddings` — embedding-integratie (zie `docs/embeddings-proposal.md`)
- `src/tray`, `src/autostart`, `src/lock`, `src/com` — tray, autostart, single-instance, COM-interop

Sterk: volledige controle, geen externe engine-afhankelijkheid, eigen ruisfilter-IP, Windows-native performance.
Zwak: alles zelf onderhouden, Windows-only, geen UI.

### chroniclecapture (TypeScript + Rust)

React/Vite/Tailwind-frontend (dashboard, views, components) met een `screenpipe-engine`-map: een eigen Rust-engine met SQL-migrations, gebaseerd rond Screenpipe.

Sterk: heeft een UI, engine draait ook in Rust, SQL-migrations aanwezig.
Zwak: twee ecosystems in één repo, afhankelijk van een extern engine-concept, minder controle over de capture-laag.

## Analyse

| Laag | chronicle-rs | chroniclecapture |
|---|---|---|
| Capture (schermtekst) | Eigen: UIA + OCR + 4-laags ruisfilter | Screenpipe-achtige engine |
| Opslag & API | Eigen `store` + `server` | SQL-migrations in engine |
| Presentatie | Geen (tray-only) | React-frontend |

De consolidatie is dus geen keuze tussen twee talen, maar **wie de capture-laag levert**. chronicle-rs is inhoudelijk het sterkst: het vierlagen-ruisfilter en de UIA-eerst-aanpak (tekst vóór beeld) past bij het Chronicle Manifest — "neem niets aan, behalve objectieve feiten": gestructureerde tekst eerst, beeld pas als fallback.

## Voorstel: één repo, drie modules

Consolideer in **chronicle-rs** als thuisbasis, met chroniclecapture's frontend als aparte module:

```
chronicle-rs/
├── engine/          # bestaande Rust-capture (uia, ocr, filter, store, server)
├── migrations/      # overgenomen uit screenpipe-engine
└── app/             # React-frontend uit chroniclecapture (Vite/Tailwind)
```

### Stappenplan

1. **Beslissen:** bevestig chronicle-rs als canonieke engine en chroniclecapture als te-archiveren repo (na migratie).
2. **Migrations overzetten:** screenpipe-engine's SQL-migrations naast chronicle-rs' eigen store; kies één schema. Betrek hier ook de `chronicle_knowledge_engine_schema.sql` (Drive).
3. **Server-API stabiliseren:** definieer de REST/API-contract van `src/server` zodanig dat de React-frontend er direct tegen kan praten.
4. **Frontend aansluiten:** chroniclecapture's views/components verhuizen naar `app/` en praten met de engine-API.
5. **Embeddings: één plan:** `docs/embeddings-proposal.md` is de enige roadmap voor semantisch zoeken.
6. **Archiveren:** chroniclecapture repository archiveren, met verwijzing naar chronicle-rs.

### Overname per repo

| Uit chronicle-rs | Uit chroniclecapture |
|---|---|
| Volledige capture-engine (UIA, OCR, ruisfilter) | React-UI met dashboard/views |
| Store + server | SQL-migrations-benadering |
| Tray, autostart, lock | Tailwind-styling en componenten |
| Embeddings-proposal | — |

## Risico's & aandachtspunten

- **Scope van "screenpipe":** als chroniclecapture's engine diep verweven is met Screenpipe-upstream, is "zelf onderhouden" (chronicle-rs) vs. "meeliften met upstream" een strategische keuze. Zelf onderhouden past beter bij het manifest, maar kost meer tijd.
- **Windows-only:** UIA is Windows-specifiek; documenteer dit als expliciete scope-keuze (of plan later een capture-trait met per-OS implementaties).
- **Eén schema:** twee eigen opslagmodellen betekent tijdelijk dubbele data-formaten — migreer in één keer, niet geleidelijk.
- **Naamgeving:** overweeg de module in chronicle-rs gewoon `capture` te laten heten (bestaat al) zodat de naamruimte één-talig blijft.

## Openstaande beslissing

1. Capture-laag volledig zelf onderhouden (chronicle-rs) of meeliften met Screenpipe-upstream?
2. Bevestiging van dit voorstel als beslissingsdocument: **chronicle-rs = engine, chroniclecapture = UI-donor**.
