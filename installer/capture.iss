; Capture — installer voor één gebruiker (Inno Setup 6.3 of nieuwer).
;
; Bouwen: .\installer\build.ps1
; Of rechtstreeks: "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\capture.iss
; Resultaat: dist\Capture-<versie>-setup.exe
;
; Bewust per gebruiker. Capture bewaart zijn data in %LOCALAPPDATA%\Capture en
; zet zijn autostart in HKCU\...\Run; een installatie onder Program Files zou
; alleen admin-rechten vragen voor iets wat de app nergens nodig heeft.
;
; De versie staat hier én in Cargo.toml. CI (en build.ps1) weigert te bouwen
; als ze verschillen, zodat setup.exe en `capture --version` nooit uiteenlopen.

#define AppName "Capture"
#define AppVersion "0.1.0"

[Setup]
; AppId is de identiteit voor upgrades en uninstall. Nooit wijzigen: een andere
; waarde ziet Windows als een tweede, losstaande applicatie.
AppId={{748DBD54-F8C8-4D35-B68B-669B0E747F37}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; lowest => {autopf} is %LOCALAPPDATA%\Programs en er is geen UAC-prompt.
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename={#AppName}-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Upgraden boven een draaiende Capture: Restart Manager sluit het proces, zodat
; de file-lock (src/lock.rs) vrijkomt voordat capture.exe vervangen wordt.
CloseApplications=yes
RestartApplications=no
UninstallDisplayIcon={app}\capture.exe
UninstallDisplayName={#AppName}
; Inno vult FileVersion alleen bij een vierdelig versienummer; zonder deze
; regel blijft dat veld leeg in het Eigenschappen-venster van setup.exe.
VersionInfoVersion={#AppVersion}.0

[Files]
; Eén binary, niets ernaast: ONNX Runtime zit er statisch in, de tray-icoon
; wordt in het geheugen getekend en de MSVC-runtime is static gelinkt.
Source: "..\target\release\capture.exe"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: "desktopicon"; Description: "Snelkoppeling op het bureaublad"; Flags: unchecked

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\capture.exe"; Parameters: "start"; Comment: "Opnemen, dashboard en tray-icoon starten"
Name: "{group}\{#AppName}-dashboard"; Filename: "http://127.0.0.1:7331"; Comment: "Het dashboard in je browser"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\capture.exe"; Parameters: "start"; Tasks: desktopicon

[Run]
Filename: "{app}\capture.exe"; Parameters: "start"; Description: "{#AppName} nu starten"; Flags: nowait postinstall skipifsilent
; Autostart hoort bij de app, niet bij de installer: `capture autostart enable`
; schrijft de HKCU-waarde met het pad van deze installatie erin. Daarom staat
; het vinkje standaard uit en duiken we niet in de registry.
Filename: "{app}\capture.exe"; Parameters: "autostart enable"; Description: "{#AppName} automatisch starten bij het inloggen"; Flags: runhidden postinstall unchecked

[UninstallRun]
; Eerst de Run-sleutel opruimen, anders wijst die na de uninstall naar een
; verdwenen exe. Daarna een harde stop als Capture nog draait: zonder dat
; kan capture.exe niet verwijderd worden en blijft er een dode binary staan.
Filename: "{app}\capture.exe"; Parameters: "autostart disable"; Flags: runhidden skipifdoesntexist; RunOnceId: "AutostartUitzetten"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM capture.exe /F"; Flags: runhidden; RunOnceId: "CaptureStoppen"

; Geen [UninstallDelete] op %LOCALAPPDATA%\Capture: database, frames en
; configuratie zijn de geschiedenis van de gebruiker, niet van de installer.
