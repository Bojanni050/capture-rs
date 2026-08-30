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
| **1. Poort** | Mag dit venster überhaupt? | Idle (geen toets/muis), wachtwoordkluizen, incognito-vensters |
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
chronicle start --no-server   # alleen opnemen
chronicle serve               # alleen de webinterface

chronicle search "factuur" --since 30d --app outlook
chronicle search "" --since 2h --kind image   # wat is er beeld geworden?
chronicle stats --since 7d
chronicle purge --older-than 60d --yes
chronicle config --init       # schrijf alle instellingen naar een bestand
```

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
  server/     axum: JSON-API en de ingebouwde webpagina
  pipeline.rs de opnamelus die alles aan elkaar knoopt
```

## Privacy

Alles blijft lokaal: SQLite en JPEG's onder
`%LOCALAPPDATA%\ChronicleCapture\data`. Er gaat niets naar buiten en er zit geen
telemetrie in.

Wat je zelf moet weten: dit legt vast wat er op je scherm staat. De denylist en
de redactie vangen de voor de hand liggende gevallen af, maar niet alles.
Controleer `app_denylist` voordat je dit langere tijd laat draaien, en gebruik
`chronicle purge` als je iets kwijt wilt.
