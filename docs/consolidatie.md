# Consolidatievoorstel: capture-rs & capture-ui

> Status: voorstel ter beslissing · Aangemaakt: 2026-09-18

## Doel

Eén activiteitenopname-oplossing ("wat gebeurt er op de computer") in plaats van twee parallelle repo's.

## Wat er nu is

### capture-rs (Rust, Windows)

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

### capture-ui (TypeScript + Rust)

React/Vite/Tailwind-frontend (dashboard, views, components) met een `screenpipe-engine`-map: een eigen Rust-engine met SQL-migrations, gebaseerd rond Screenpipe.

Sterk: heeft een UI, engine draait ook in Rust, SQL-migrations aanwezig.
Zwak: twee ecosystems in één repo, afhankelijk van een extern engine-concept, minder controle over de capture-laag.

## Analyse

| Laag | capture-rs | capture-ui |
|---|---|---|
| Capture (schermtekst) | Eigen: UIA + OCR + 4-laags ruisfilter | Screenpipe-achtige engine |
| Opslag & API | Eigen `store` + `server` | SQL-migrations in engine |
| Presentatie | Geen (tray-only) | React-frontend |

De consolidatie is dus geen keuze tussen twee talen, maar **wie de capture-laag levert**. capture-rs is inhoudelijk het sterkst: het vierlagen-ruisfilter en de UIA-eerst-aanpak (tekst vóór beeld) past bij het Capture Manifest — "neem niets aan, behalve objectieve feiten": gestructureerde tekst eerst, beeld pas als fallback.

## Voorstel: één repo, drie modules

Consolideer in **capture-rs** als thuisbasis, met capture-ui's frontend als aparte module:

```
capture-rs/
├── engine/          # bestaande Rust-capture (uia, ocr, filter, store, server)
├── migrations/      # overgenomen uit screenpipe-engine
└── app/             # React-frontend uit capture-ui (Vite/Tailwind)
```

### Stappenplan

1. **Beslissen:** bevestig capture-rs als canonieke engine en capture-ui als te-archiveren repo (na migratie).
2. **Migrations overzetten:** screenpipe-engine's SQL-migrations naast capture-rs' eigen store; kies één schema. Betrek hier ook de `capture_knowledge_engine_schema.sql` (Drive).
3. **Server-API stabiliseren:** definieer de REST/API-contract van `src/server` zodanig dat de React-frontend er direct tegen kan praten.
4. **Frontend aansluiten:** capture-ui's views/components verhuizen naar `app/` en praten met de engine-API.
5. **Embeddings: één plan:** `docs/embeddings-proposal.md` is de enige roadmap voor semantisch zoeken.
6. **Archiveren:** capture-ui repository archiveren, met verwijzing naar capture-rs.

### Overname per repo

| Uit capture-rs | Uit capture-ui |
|---|---|
| Volledige capture-engine (UIA, OCR, ruisfilter) | React-UI met dashboard/views |
| Store + server | SQL-migrations-benadering |
| Tray, autostart, lock | Tailwind-styling en componenten |
| Embeddings-proposal | — |

## Risico's & aandachtspunten

- **Scope van "screenpipe":** als capture-ui's engine diep verweven is met Screenpipe-upstream, is "zelf onderhouden" (capture-rs) vs. "meeliften met upstream" een strategische keuze. Zelf onderhouden past beter bij het manifest, maar kost meer tijd.
- **Windows-only:** UIA is Windows-specifiek; documenteer dit als expliciete scope-keuze (of plan later een capture-trait met per-OS implementaties).
- **Eén schema:** twee eigen opslagmodellen betekent tijdelijk dubbele data-formaten — migreer in één keer, niet geleidelijk.
- **Naamgeving:** overweeg de module in capture-rs gewoon `capture` te laten heten (bestaat al) zodat de naamruimte één-talig blijft.

## Openstaande beslissing

1. Capture-laag volledig zelf onderhouden (capture-rs) of meeliften met Screenpipe-upstream?
2. Bevestiging van dit voorstel als beslissingsdocument: **capture-rs = engine, capture-ui = UI-donor**.

## Platform-strategie capture (mobiel)

Het digitale leven volledig in kaart brengen vereist op termijn ook Android- en iOS-clients. De haalbaarheid verschilt fundamenteel per platform — continue screen-capture zoals op Windows (UIA + OCR) bestaat op mobiel niet.

### Android — beperkt mogelijk

- **Accessibility Service**: kan schermtekst lezen (dichtstbijzijnde tegenhanger van UIA). Kanttekening: Google weigert Play Store-toelating voor logging-gebruik zonder duidelijke gebruikersfunctie; side-loading of eigen distributie is vaak de realiteit.
- **UsageStatsManager**: app-gebruik per tijdsinterval (metadata, geen inhoud).
- **Share-sheet / Intent**: gebruiker deelt tekst, links of bestanden handmatig.
- Foreground screen recording + on-device OCR kan, maar is batterij-intensief.

### iOS — vrijwel onmogelijk voor continue capture

- Geen publieke API om inhoud van andere apps op de achtergrond te lezen; geen Accessibility Service, geen scherm-scraping, geen globale OCR.
- Wél mogelijk: handmatige **share-extension**, **Shortcuts/Automations** (met gebruikersbevestiging), en aggregaten via **DeviceActivityFramework** (app-namen en tijden, geen inhoud).

### Drielaagse mobiele strategie

1. **Direct**: share-sheet/Shortcuts-capture — de mens geeft expliciet door wat ertoe doet (past bij het manifest: menselijke intentie als bron).
2. **Metadata**: app-gebruik en tijden via UsageStats/DeviceActivity.
3. **Indirect (grootste vangst)**: automatische import uit cloud-bronnen — Gmail, Drive, agenda's, notities. Het meeste mobiele gedrag laat sporen na in de cloud; die zijn via API's wél volledig toegankelijk.

Mobiele clients worden binnen de Gaia-architectuur representaties van dezelfde Gaia in Gaia Cloud; alleen de capture-methode verschilt per platform. De engine-API (stap 3 van het stappenplan) moet daarom platform-neutraal zijn: clients leveren gestandaardiseerde observaties aan, ongeacht of die via UIA, share-sheet of cloud-import zijn verkregen.
## Ingestie-architectuur: één convergentiepunt

Alle observatiestromen — desktop-capture, mobiele capture (Android/iOS), AI-chatarchieven en toekomstige bronnen — komen samen in **één ingestiepunt** in de engine. Dit is de logische plek waar capture-rs' server-API en de knowledge-engine elkaar raken.

### Waarom één punt

- **Eén statusmarkering**: alle input doorloopt dezelfde Trust-domein-controle (observation/interpretation/hypothesis/...) op één plek, in plaats van per client geïmplementeerd — het manifest verbiedt immers dat interpretaties als feiten naar buiten treden.
- **Eén ruisfilter**: het vierlagen-filter en deduplicatie werken op alle bronnen uniform, ongeacht of een observatie via UIA, share-sheet of cloud-import binnenkomt.
- **Eén eventing/schema**: clients hoeven alleen gestandaardiseerde observaties te sturen; alle verwerking (pipeline, embeddings, opslag) is brononafhankelijk.

### Conceptueel model

```
[desktop capture]   [mobiel capture]   [AI-chatarchieven]   [cloud-import]
       │                   │                  │                  │
       └───────────────────┴────────┬─────────┴──────────────────┘
                                   ▼
                        ┌─────────────────────┐
                        │   Ingestie Gateway   │  (één API, één contract)
                        │  - normalisatie      │
                        │  - deduplicatie      │
                        │  - statusmarkering   │
                        │  - ruisfilter        │
                        └──────────┬──────────┘
                                   ▼
                        ┌─────────────────────┐
                        │  Pipeline / Store    │
                        │  (kennisvorming)     │
                        └─────────────────────┘
```

### Ontwerpeisen

- **Contract-first**: definieer eerst het observatie-formaat (source, timestamp, content, context, confidence) vóórdat clients worden gebouwd — dit is de concrete invulling van "platform-neutraal" uit de vorige sectie.
- **Bron-provenance**: elke observatie draagt zijn herkomst (device, app, capture-methode) mee — nodig voor latere bias-analyse en het Bias Inference Framework.
- **Push én pull**: desktop pusht continue; mobiele clients pushen opportunistisch (batterij/connectiviteit); cloud-import trekt periodiek. De gateway moet beide patronen aankunnen.
- **Toekomstbestendig**: nieuwe bronnen (bijv. email, muziekgedrag, locatie) zijn alleen nieuwe adapters, geen architectuurwijziging.
