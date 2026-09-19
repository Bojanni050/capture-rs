# Chronicle

Houdt bij wat je op je pc doet — in Rust, lokaal, zonder cloud. Geïnspireerd op
[screenpipe](https://github.com/screenpipe/screenpipe), maar toegespitst op
Windows en met een ruisfilter dat serieus werk doet in plaats van alles te
bewaren.

Bij een wissel van voorgrondvenster reageert Chronicle direct (na een korte
debounce); daarnaast blijft er een periodieke controle als vangnet voor video,
canvas-apps en andere inhoud zonder Windows-events. Daarna leest het de tekst
uit, filtert ruis en slaat het resultaat doorzoekbaar op. Dat uitlezen gebeurt
via drie bronnen, in volgorde van betrouwbaarheid:

1. **UI Automation** — de tekens die de app zelf aan schermlezers geeft. Exact,
   geen leesfouten.
2. **OCR** — pixels lezen met de engine die in Windows zit. Werkt overal, maar
   raadt soms verkeerd, dus er gaat een kwaliteitsdrempel overheen.
3. **Beeld** — geeft geen van beide iets bruikbaars, dan bewaren we het frame
   zelf. Dat is de **image-fallback**, en die zorgt dat er nooit een gat in je
   tijdlijn valt.

## Wat het oplevert

```
$ chronicle search "kwartaalrapportage" --since 7d

12-08 14:22  outlook  [tekst]  RE: Kwartaalcijfers Q3 - Outlook
  ...de «kwartaalrapportage» moet uiterlijk vrijdag bij finance liggen...
  → http://127.0.0.1:7331/#8412

11-08 09:05  excel  [tekst]  Q3-2026.xlsx - Excel
  ...tabblad «kwartaalrapportage» bevat de definitieve marges...
  → http://127.0.0.1:7331/#8109
```

En op `http://127.0.0.1:7331` staat dezelfde zoekfunctie met een tijdlijn en de
bewaarde beelden erbij.

## Het ruisfilter

Dit is het verschil met "alles opslaan". Vier lagen, van goedkoop naar duur —
elke laag die iets afvangt, bespaart de laag erna:

| Laag | Vraag | Wat het afvangt |
|---|---|---|
| **1. Poort** | Mag dit venster überhaupt? | Idle (geen toets/muis), wachtwoordkluizen, incognito-vensters, uitgesloten apps/vensters/domeinen |
| **2. Beeld** | Is er iets veranderd? | Stilstaande schermen — een dHash-vergelijking vóór de dure OCR |
| **3. Tekst** | Wat hiervan is inhoud? | Terugkerende menubalken en statusbalken, per app afgeleerd |
| **4. Redactie** | Mag dit bewaard worden? | Creditcards, IBAN's, `password:`-regels, API-tokens |

Laag 3 is de interessantste. Het filter telt per app hoe vaak elke tekstregel
terugkomt. Wat in meer dan 60% van de frames van jouw editor staat, is de
menubalk — niet je werk. Die regels blijven wél in de bewaarde tekst staan
(zodat je een scherm compleet kunt terugkijken), maar gaan níét de zoekindex in.
Dat geheugen overleeft een herstart.

Wat het filter heeft weggegooid en waarom, zie je terug in `chronicle stats`.

## De accessibility-laag

Windows houdt voor schermlezers al een boom bij van wat er op je scherm staat:
knoppen, labels, tekstvelden, links. Die uitlezen geeft de **echte tekens** in
plaats van een gok op basis van pixels — geen `l` die een `1` wordt, geen
`socket` die `socker` wordt.

De valkuil is het aantal COM-aanroepen. Knoop-voor-knoop door de boom lopen kost
één cross-process call per property, en dan ben je trager dan OCR. Chronicle
haalt daarom de hele deelboom in **één** `FindAllBuildCache` op, met de
properties vooraf gedeclareerd, en leest daarna alleen nog uit de cache. De
wandeling gebruikt de *content view*, niet de raw view: dat laat elk decoratief
paneel en elke scrollbar links liggen.

**Wat je er in de praktijk van mag verwachten** (gemeten op een 2560×1440-scherm):

| App | knopen | tekens | tijd |
|---|---:|---:|---:|
| Notepad | 41 | 170 | 380 ms |
| Edge (leeg tabblad) | 38 | 110 | 240 ms |
| Explorer | 152 | 669 | 980 ms |
| Brave (artikelpagina) | 779 | 3166 | 1785 ms |
| Electron-app | 560 | 1572 | 2450 ms |
| PowerShell | 1 | 0 | 6 ms |

Ter vergelijking: OCR over datzelfde volledige scherm kost **290–510 ms** en
levert ~1700 tekens met kwaliteit 0.80. UIA is dus niet altijd sneller — voor
zware vensters is het juist trager. Wat het wél oplevert is exactheid, en op een
volle browserpagina ook méér tekst dan OCR eruit haalt.

Drie dingen om te weten:

- **Chromium-apps schakelen accessibility geleidelijk in.** De eerste keer dat
  je een Electron-app of browser bevraagt, krijg je nog niets; daarna loopt het
  binnen een paar metingen op (in de test: 184 → 645 → 1601 → 3166 tekens). De
  drempel `min_text_len` houdt die magere eerste oogst uit je archief.
- **Een UIA-aanroep kan hangen** als de doelapp vastzit. De lezer draait daarom
  op een eigen thread met een deadline; loopt die af, dan gaat OCR verder en
  slaat de volgende tik UIA over zolang de thread nog bezet is.
- **Niet elke app doet mee.** PowerShell en sommige Electron-apps geven een lege
  boom. Chronicle onthoudt dat per app en zet UIA daar tijdelijk uit, zodat je
  niet elke tik opnieuw voor niets wacht.

`chronicle doctor` probeert UIA op al je open vensters en zegt per app of het
werkt — de snelste manier om te zien wat jouw mix oplevert.

## De image-fallback

Na OCR krijgt de tekst een kwaliteitsscore (0–1): hoeveel alfanumeriek, hoeveel
woorden zien er als échte woorden uit, hoeveel losse rommeltekens. Echte tekst
scoort ~0.8, OCR-ruis op een video of een spel scoort ~0.3.

- **Genoeg tekst en hoog genoeg scorend** → opgeslagen als tekst, doorzoekbaar.
- **Te weinig of te rommelig** → het frame zelf wordt als JPEG bewaard, met de
  reden erbij ("lage tekstkwaliteit (0.24 < 0.38)").

Die kwaliteitsdrempel geldt alleen voor OCR. UIA-tekst komt rechtstreeks uit de
app en bevat per definitie geen leesfouten, dus daar wordt alleen gekeken of er
ná het ruisfilter nog genoeg inhoud over is.

Zo valt er nooit een gat in je tijdlijn: je hebt óf de tekst, óf het beeld. Je
ziet de reden terug in de webinterface bij elk beeld-item.

## Wachtwoordvelden en de uitsluitingslijst

Twee signalen leren Chronicle wat het nooit mag vastleggen, zonder dat jij een
lijst met banken bijhoudt:

- **UIA `IsPassword`** — staat er ergens in de accessibility-boom van het
  venster een wachtwoordveld (ook buiten beeld gescrolld), dan wordt er niets
  van dat venster bewaard: geen tekst, geen OCR, geen beeld, en ook het lege
  segment met de venstertitel wordt weer weggegooid. Het **venster** (`app::titel`)
  komt op de uitsluitingslijst, niet de hele app: één instellingenscherm met een
  API-sleutelveld mag Word of je mailclient niet voorgoed stilleggen.
- **Browserextensie** — UIA ziet in een browser alleen een wachtwoordveld
  zolang het in beeld is, en weet niet welke site het is. De extensie in
  `browser-extension/` meldt de hostnaam van het actieve tabblad en of de
  DOM een `input[type=password]` heeft. Daarmee wordt het hele **domein**
  uitgesloten, dus ook de pagina's ná het inloggen. Zie
  [`browser-extension/README.md`](browser-extension/README.md) voor installeren.
  Zonder extensie werkt in browsers alleen de UIA-controle (per venstertitel).

Uitsluiten is **sticky**: wat er eenmaal op staat blijft staan tot jij het
weghaalt. Een dashboard dat na het inloggen geen wachtwoordveld meer toont mag
niet stilletjes weer meedoen.

```bash
chronicle exclude list
chronicle exclude add domain mijnbank.nl
chronicle exclude add app slack
chronicle exclude remove domain github.com   # bv. als het te grof bleek
```

Beperking: bij `monitor = "all"` staat UIA uit (zie hieronder), dus dan is er
geen wachtwoordveld-detectie via UIA; alleen de browserextensie en de denylists
werken dan nog.

## Alleen het voorgrondvenster, nooit wat eromheen staat

Een screenshot van het hele scherm laat ook zien wat er ván een niet-
gemaximaliseerd venster nog zichtbaar is — een ander tabblad, een chatvenster
op de achtergrond, een sidebar. Zonder maatregel zou dat allemaal door OCR
gehaald en aan de verkeerde app toegeschreven worden.

Daarom snijdt Chronicle elke screenshot vóór OCR (en vóór een eventueel bewaard
beeld) bij tot het zichtbare rechthoek van het voorgrondvenster
(`DwmGetWindowAttribute`/`DWMWA_EXTENDED_FRAME_BOUNDS`, met `GetWindowRect` als
terugval). Dat rechthoek wordt vlak vóór de screenshot opgevraagd, en er komt
een re-check ná de screenshot: wisselde het voorgrondvenster ondertussen, dan
wordt die tik overgeslagen in plaats van een verouderde rechthoek op de
verkeerde pixels toe te passen. Bij meerdere schermen (`monitor = "all"`)
betekent dit ook dat een scherm waar het venster niet op staat helemaal niet
meer meegelezen wordt.

**UI Automation heeft dit probleem structureel niet** — het leest de
accessibility-boom die geworteld is in één specifiek venster, dus content van
een ander venster kan daar nooit in lekken. De bijsnijding is dus vooral van
belang voor de OCR-fallback, en voor de beeld-fallback wanneer die wordt
opgeslagen.

Dit vereist dat Chronicle per-monitor DPI-bewust is (`main.rs` zet dit bij het
opstarten) — zonder dat geeft Windows geschaalde coördinaten terug die niet
meer overeenkomen met de fysieke pixels van een screenshot, en zou het
bijsnijden het verkeerde stuk scherm pakken.

## Installatie

Nodig: Rust (stable, msvc-toolchain), Windows 10/11, en minstens één taalpakket
met OCR-ondersteuning (`chronicle doctor` vertelt je of dat er is).

```bash
cargo build --release
```

De binary staat in `target/release/chronicle.exe`.

## Gebruik

```bash
chronicle doctor              # controleer uia per app, OCR, schermen, database
chronicle start               # opnemen + webinterface op 127.0.0.1:7331
chronicle start --tray        # hetzelfde, plus een systemtray-icoon
chronicle start --no-server   # alleen opnemen
chronicle serve               # alleen de webinterface

chronicle search "factuur" --since 30d --app outlook
chronicle search "" --since 2h --kind image   # wat is er beeld geworden?
chronicle stats --since 7d
chronicle purge --older-than 60d --yes
chronicle config --init       # schrijf alle instellingen naar een bestand

chronicle autostart enable    # start automatisch op bij het inloggen (met tray-icoon)
chronicle autostart disable
chronicle autostart status
```

## Systemtray-icoon en automatisch opstarten

`chronicle start --tray` toont een stip in de systemtray die de status van de
opname laat zien:

| Kleur | Betekent |
|---|---|
| 🟢 groen | opname actief |
| 🟡 geel | je bent even weg (idle) |
| 🔴 rood | de laatste tik mislukte |

Rechtsklik erop voor het dashboard, een vinkje voor automatisch opstarten en
"Chronicle afsluiten". Dubbelklikken opent meteen het dashboard.

Voor automatisch opstarten bij het inloggen, zonder het icoon zelf elke keer
aan te hoeven zetten:

```bash
chronicle autostart enable
```

Dit zet `chronicle.exe start --tray` in de `Run`-sleutel van je eigen
Windows-account (`HKCU\...\Run`) — geen adminrechten nodig, en het start pas
ná inloggen (dus met je bureaublad en schermen al actief). `chronicle
autostart disable` zet het weer uit; `chronicle autostart status` laat zien
wat er nu staat.

`doctor` probeert UIA op al je open vensters én doet een echte OCR-proefopname,
met tijden erbij — de snelste manier om je drempels te ijken.

## Configuratie

`chronicle config --init` schrijft alle defaults naar
`%LOCALAPPDATA%\ChronicleCapture\chronicle.toml`. De knoppen die er het meest
toe doen:

```toml
[capture]
interval_secs = 4.0        # tempo terwijl je werkt
idle_interval_secs = 60.0  # tempo terwijl je weg bent
idle_after_secs = 90       # wanneer je als "weg" telt
monitor = "primary"        # of "all", of een index

[uia]
enabled = true               # accessibility-boom als primaire bron
min_text_len = 120           # minder tekens dan dit -> doorschuiven naar OCR
timeout_ms = 2500            # daarna gaat OCR verder; de boom mag doorwerken
failures_before_skip = 3     # daarna gaat uia voor die app tijdelijk uit
retry_after_secs = 600       # en krijgt hij daarna weer een kans
app_denylist = []            # apps waarvoor je uia nooit wilt proberen

[ocr]
min_text_len = 24          # minder tekens dan dit → beeld-fallback
min_quality = 0.38         # lagere score dan dit → beeld-fallback

[filter]
phash_threshold = 4        # hoger = meer frames gelden als "onveranderd"
boilerplate_ratio = 0.6    # regel in >60% van de frames = vaste UI
app_denylist = ["bitwarden", "keepass", "1password", ...]
redact = true

[browser]
enabled = true             # bridge voor de browserextensie
port = 8765                # zie browser-extension/README.md
max_age_secs = 15.0        # oudere meldingen vertrouwen we niet

[ship]
enabled = false            # true = gefilterde tekst naar je Stash sturen
endpoint = "http://100.64.144.93:8080/ingest"
interval_secs = 300.0      # token: env STASH_AUTH_TOKEN of auth_token = "..."

[storage]
keep_frames = "fallback"   # "always" | "fallback" | "never"
retention_days = 45        # alles ouder dan dit verdwijnt
frame_retention_days = 10  # afbeeldingen eerder, tekst blijft langer
```

Bij `monitor = "all"` behandelt Chronicle elk scherm als een eigen bron: elk
frame krijgt eerst zijn eigen beeldhash en tekstvergelijking. UI Automation
beschrijft alleen het voorgrondvenster en wordt daarom alleen gebruikt wanneer
één monitor is geselecteerd; bij een multischerm-opname leest OCR de schermen
apart. Zo kan tekst op één scherm een tweede scherm niet wegfilteren.

Retentie draait automatisch elk uur terwijl `start` loopt.

## API

De webserver bindt op 127.0.0.1 en heeft **geen authenticatie** — de aanname is
dat dit alleen op je eigen machine bereikbaar is. Zet je `bind` breder open, doe
dat dan achter een proxy die de toegang regelt.

| Endpoint | Wat het geeft |
|---|---|
| `GET /api/search?q=&app=&kind=&from=&to=&limit=` | Zoekresultaten met fragment |
| `GET /api/timeline?from=&to=` | Vensters in een tijdvak, met duur |
| `GET /api/stats?from=&to=` | Aantallen, top-apps, wat het filter afving |
| `GET /api/capture/{id}` | Eén capture, volledige tekst en de bron (`uia`/`ocr`) |
| `GET /api/frame/{id}` | De bewaarde afbeelding (JPEG) |
| `GET /api/apps` | Alle apps die zijn gezien |

Bruikbaar om een LLM je eigen geschiedenis te laten bevragen:

```bash
curl "http://127.0.0.1:7331/api/search?q=deployment&from=$(date -d '2 days ago' +%s)"
```

## Waar staat wat

```
src/
  capture/    screenshot (xcap), actief venster (Win32), idle-detectie
  uia/        UI Automation: de accessibility-boom, met timeout en per-app geheugen
  ocr/        Windows.Media.Ocr op een eigen thread
  filter/     de vier lagen: dedupe, text, privacy, en mod.rs die ze aanstuurt
  store/      SQLite + FTS5, en frames als JPEG op schijf
  embeddings/ semantische laag (experimenteel, uit by default — zie hieronder)
  server/     axum: JSON-API en de ingebouwde webpagina
  browser.rs  bridge naar de browserextensie (alleen extensies, alleen 127.0.0.1)
  ship.rs     optionele shipper naar Stash, met cursor in SQLite
  pipeline.rs de opnamelus die alles aan elkaar knoopt
browser-extension/  MV3-extensie: domein + wachtwoordveld van het actieve tabblad
deploy/
  stash-ingest/    ruwe buffer op je VPS (Flask + SQLite, alleen Tailscale)
  selection-pass/  LLM-relevantiefilter: Stash -> Hindsight
```

## Stash en Hindsight (optioneel)

Standaard blijft alles op deze machine. Wil je ook een curatiestraat, zet dan
`[ship] enabled = true`. Chronicle stuurt elke 5 minuten de nieuwe
`index_text` (dus zonder terugkerende menubalken en zonder gevoelige patronen)
als NDJSON naar `deploy/stash-ingest`; `deploy/selection-pass` bundelt dat op
je VPS tot episodes, laat een LLM beslissen wat het onthouden waard is en
bewaart de rest in Hindsight. Installatie van beide staat in hun eigen README.

- De **eerste keer** begint de shipper bij de nieuwste capture, niet bij de
  hele historie.
- Een cursor in SQLite schuift pas op nadat Stash de batch met 2xx bevestigde;
  bij een storing wordt dezelfde batch de volgende ronde opnieuw verstuurd.
- Alleen `http://`: het is bedoeld voor Tailscale. Het token komt uit de
  omgevingsvariabele `STASH_AUTH_TOKEN` (of `ship.auth_token`).
- Stash dedupliceert op (tijdstip, app, venstertitel); bij `monitor = "all"`
  kunnen twee schermen met dezelfde titel binnen dezelfde seconde dus één rij
  worden.

## Semantisch zoeken (experimenteel)

Naast FTS5 zit er een `[embeddings]`-sectie in de config (`enabled = false`
standaard) die captures groepeert, embedt en in pgvector opslaat voor
`chronicle search --semantic`. Met `provider = "fastembed"` gebruikt dat een
echt lokaal model (`intfloat/multilingual-e5-small`, ONNX Runtime, CPU,
NL+EN) — geen cloud-aanroep, wel een eenmalige download van ~118 MB bij een
lege modelcache. De default blijft `provider = "mock"`, dus `enabled = true`
alleen triggert nooit ongevraagd die download.

Dit is nog geen afgeronde feature: zonder een draaiende Postgres/pgvector
deelt geen enkele losse CLI-aanroep (`chronicle search --semantic`,
`chronicle embeddings status/rebuild`) zijn data met een lopend
`chronicle start`-proces — alleen de ingebouwde webinterface van dat proces
zelf ziet wat er geïndexeerd is. Zie
[`docs/embeddings-proposal.md`](docs/embeddings-proposal.md) voor de
volledige architectuur en een expliciete lijst bekende beperkingen voordat je
het aanzet.

## Privacy

Standaard blijft alles lokaal: SQLite en JPEG's onder
`%LOCALAPPDATA%\ChronicleCapture\data`. Er gaat niets naar buiten en er zit geen
telemetrie in. De enige uitzondering is de shipper (`[ship] enabled = true`),
die uit staat tot jij hem aanzet en dan alleen gefilterde tekst naar jouw eigen
Stash stuurt.

Wat je zelf moet weten: dit legt vast wat er op je scherm staat. De denylist en
de redactie vangen de voor de hand liggende gevallen af, maar niet alles.
Controleer `app_denylist` voordat je dit langere tijd laat draaien, en gebruik
`chronicle purge` als je iets kwijt wilt.
