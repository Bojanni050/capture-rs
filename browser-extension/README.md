# Chronicle Browser Bridge

Meldt de hostnaam van het actieve tabblad en of de pagina een
`input[type="password"]` bevat aan de lokale Chronicle-opname, via
`http://127.0.0.1:8765/browser-status`. Daardoor kan Chronicle per **domein**
uitsluiten: `mijnbank.com` blijft uitgesloten, ook op pagina's van die site die
op dat moment geen loginformulier tonen. De UIA-`IsPassword`-controle van
Chronicle vangt alleen een wachtwoordveld op het moment dat het zichtbaar is.

Dit vervangt die UIA-controle niet — het is een eerdere, grovere laag. Is de
extensie niet geïnstalleerd, dan blijft de opname werken; de meldingen mislukken
dan stil.

## Installeren (unpacked, voor eigen gebruik)

1. `chrome://extensions` (of `edge://extensions`) → **Ontwikkelaarsmodus** aan
2. **Uitgepakte extensie laden** → kies deze map `browser-extension/`
3. Zorg dat `chronicle start` draait; die opent de bridge op poort 8765

Werkt in elke Chromium-browser (Chrome, Edge, Brave, Opera, Vivaldi). Firefox
heeft een eigen port nodig; zonder extensie werkt daar alleen de UIA-controle.

## Van wie accepteert de bridge meldingen?

Alleen van extensies (`Origin: chrome-extension://…`) of van clients zonder
`Origin` (zoals `curl`). Een gewone webpagina die `127.0.0.1:8765` probeert aan
te spreken krijgt een 403 — anders zou elke site je uitsluitingslijst kunnen
vervuilen of zich als een ander domein kunnen voordoen.

## De poort veranderen

Zet je `[browser] port` in `chronicle.toml` om, pas dan zowel `BRIDGE_URL` in
`background.js` als de `http://127.0.0.1:8765/*`-regel in `host_permissions` van
`manifest.json` aan en herlaad de extensie.
