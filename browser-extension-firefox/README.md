# Capture Browser Bridge — Firefox

Zelfde functie als `../browser-extension/`, maar dan voor Firefox:

meldt de hostnaam van het actieve tabblad en of de pagina een
`input[type="password"]` bevat aan de lokale Capture-opname, via
`http://127.0.0.1:8765/browser-status`. Zie de README van de
Chromium-variant voor het volledige verhaal.

Verschillen met de Chromium-versie:

- Firefox MV3 kent geen service workers: `background.js` draait hier
  als event-page-script.
- De API-namespace heet `browser.*`; de scripts vallen terug op
  `chrome.*` zodat één bestand in beide browsers kan werken.

## Installeren (tijdelijk, voor eigen gebruik)

1. `about:debugging#/runtime/this-firefox` → **Load Temporary Add-on…**
2. Kies `manifest.json` in deze map
3. Zorg dat `capture start` draait; die opent de bridge op poort 8765

Een tijdelijke add-on verdwijnt weer bij herstarten van Firefox.
Permanente installatie vereist ondertekening via
addons.mozilla.org — voor eigen gebruik is tijdelijk laden genoeg.

## Statusindicator

Klik op het extensie-icoon voor de popup met de actuele stand,
identiek aan de Chromium-variant. Zie `../browser-extension/README.md`.

## De poort veranderen

Zet je `[browser] port` in `capture.toml` om, pas dan zowel
`BRIDGE_URL` in `background.js` als de `http://127.0.0.1:8765/*`-regel
in `host_permissions` van `manifest.json` aan en herlaad de add-on.
