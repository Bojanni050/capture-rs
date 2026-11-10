# Installer

`capture.iss` bouwt één `setup.exe` voor de huidige gebruiker: geen admin, geen
service, geen machine-brede registry. Dat past bij de app — die bewaart alles in
`%LOCALAPPDATA%\Capture` en zet zijn autostart in `HKCU\...\Run`.

## Bouwen

Nodig: Rust (stable, msvc-toolchain) en [Inno Setup](https://jrsoftware.org/isdl.php) 6.3 of nieuwer.

```powershell
choco install innosetup -y     # eenmalig
.\installer\build.ps1
```

Resultaat: `dist\Capture-<versie>-setup.exe`. Rechtstreeks met ISCC kan ook:

```powershell
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\capture.iss
```

De installer bevat alleen `target\release\capture.exe`. Er hoeft geen DLL of
runtime mee: ONNX Runtime is statisch gelinkt, de MSVC-runtime ook, en de
tray-icoon wordt tijdens het draaien in het geheugen getekend.

## Wat de installer doet

| Onderdeel | Gedrag |
|---|---|
| Installatiemap | `%LOCALAPPDATA%\Programs\Capture` |
| Startmenu | "Capture" (`capture.exe start`) en "Capture-dashboard" (URL naar `127.0.0.1:7331`) |
| Bureaublad | Alleen als je het vinkje aanvinkt |
| Autostart | Aangeboden als **uit**-vinkje op de laatste pagina; het is `capture autostart enable`, dus de app schrijft zelf de `Run`-sleutel met het juiste pad |
| Upgraden | `CloseApplications=yes` laat de Restart Manager een draaiende Capture sluiten, zodat de file-lock vrijkomt vóór `capture.exe` vervangen wordt |
| Deïnstalleren | Zet eerst autostart uit, stopt daarna een nog draaiende instantie en verwijdert de snelkoppelingen |

`%LOCALAPPDATA%\Capture` (database, frames, `capture.toml`, logs) blijft bij een
uninstall **staan**. Dat is je geschiedenis; wie die kwijt wil, verwijdert de map
zelf.

De browserextensie zit niet in de installer: Chrome en Edge laten unpacked
extensies niet stilzwijgend installeren. Zie `browser-extension/README.md`.

## Versie verhogen

De versie staat op twee plekken: `Cargo.toml` en `#define AppVersion` in
`capture.iss`. `build.ps1` en CI weigeren te bouwen als ze verschillen — pas dus
beide aan bij een release. Dat is bewust: ISPP-parameters (`/D`) zijn lastig
betrouwbaar te quoten vanuit elke shell, en een stille mismatch tussen
`setup.exe` en `capture --version` is vervelender dan één extra regel.

## CI

`.github/workflows/ci.yml` bouwt na de tests de binary en de installer, en zet
`dist\Capture-*-setup.exe` als artifact `capture-setup` op de build. Elke push
levert dus een installeerbare versie op, zonder release-proces.

## Ondertekening

De installer is niet ondertekend, dus SmartScreen toont "Windows heeft uw pc
beschermd" tot je een code-signing-certificaat hebt:

```powershell
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /a dist\Capture-0.1.0-setup.exe
```

Voor eigen gebruik is dat te negeren ("Meer informatie" → "Toch uitvoeren").
