//! De ingebouwde webpagina. Eén bestand, geen externe verzoeken, zodat de
//! server ook zonder internet werkt en er niets van je activiteit weglekt.

pub const PAGE: &str = r##"<!doctype html>
<html lang="nl">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Capture</title>
<style>
  :root {
    --bg: #f6f7f9; --panel: #ffffff; --line: #e2e5ea; --text: #14171c;
    --muted: #666e7a; --accent: #2f6fd0; --mark: #ffe9a8; --chip: #eef1f5;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #14171b; --panel: #1c2026; --line: #2c323a; --text: #e6e9ee;
      --muted: #98a1ad; --accent: #6ea8fe; --mark: #5c4a12; --chip: #262c34;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--text);
    font: 14px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  header {
    padding: 14px 20px; border-bottom: 1px solid var(--line);
    background: var(--panel); position: sticky; top: 0; z-index: 5;
  }
  h1 { margin: 0 0 10px; font-size: 17px; letter-spacing: -0.2px; }
  h1 span { color: var(--muted); font-weight: 400; font-size: 13px; margin-left: 8px; }
  .controls { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  input[type=search], select {
    padding: 7px 10px; border: 1px solid var(--line); border-radius: 7px;
    background: var(--bg); color: var(--text); font: inherit;
  }
  input[type=search] { flex: 1 1 280px; min-width: 200px; }
  .stats { display: flex; gap: 16px; flex-wrap: wrap; margin-top: 10px;
           color: var(--muted); font-size: 12px; }
  .stats b { color: var(--text); font-variant-numeric: tabular-nums; }
  main { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
         gap: 16px; padding: 16px 20px; align-items: start; }
  @media (max-width: 900px) { main { grid-template-columns: 1fr; } }
  .list { display: flex; flex-direction: column; gap: 8px; }
  .hit {
    background: var(--panel); border: 1px solid var(--line); border-radius: 9px;
    padding: 10px 12px; cursor: pointer;
  }
  .hit:hover { border-color: var(--accent); }
  .hit.sel { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .meta { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap;
          font-size: 12px; color: var(--muted); margin-bottom: 4px; }
  .app { color: var(--accent); font-weight: 600; }
  .title { color: var(--text); overflow: hidden; text-overflow: ellipsis;
           white-space: nowrap; max-width: 100%; }
  .kind { border: 1px solid var(--line); background: var(--chip);
          border-radius: 20px; padding: 1px 8px; font-size: 11px; }
  .snippet { color: var(--text); font-size: 13px; word-break: break-word;
             max-height: 4.6em; overflow: hidden; }
  .snippet mark { background: var(--mark); color: inherit; border-radius: 2px; }
  .detail {
    background: var(--panel); border: 1px solid var(--line); border-radius: 9px;
    padding: 14px; position: sticky; top: 128px; max-height: calc(100vh - 150px);
    overflow: auto;
  }
  .detail img { width: 100%; border-radius: 7px; border: 1px solid var(--line);
                margin-bottom: 12px; }
  .detail pre { white-space: pre-wrap; word-break: break-word; margin: 0;
                font: 12.5px/1.55 ui-monospace, "Cascadia Code", Consolas, monospace; }
  .empty { color: var(--muted); padding: 24px 4px; }
  .note { color: var(--muted); font-size: 12px; margin: 8px 0 12px;
          border-left: 2px solid var(--line); padding-left: 10px; }
  details.logs { margin-top: 10px; color: var(--muted); font-size: 12px; }
  details.logs summary { cursor: pointer; }
  #logs { margin-top: 6px; max-height: 220px; overflow: auto;
          background: var(--bg); border: 1px solid var(--line);
          border-radius: 7px; padding: 8px 10px;
          font: 11.5px/1.5 ui-monospace, "Cascadia Code", Consolas, monospace; }
  #logs .log-WARN { color: #a16207; }
  #logs .log-ERROR { color: #b91c1c; font-weight: 600; }
</style>
</head>
<body>
<header>
  <h1>Capture<span id="range-label">laatste 24 uur</span></h1>
  <div class="controls">
    <input type="search" id="q" placeholder="Zoek in alles wat je scherm liet zien…" autofocus>
    <select id="app"><option value="">alle apps</option></select>
    <select id="kind">
      <option value="">tekst en beeld</option>
      <option value="text">alleen tekst</option>
      <option value="image">alleen beeld (fallback)</option>
    </select>
    <select id="range">
      <option value="86400">laatste 24 uur</option>
      <option value="10800">laatste 3 uur</option>
      <option value="604800">laatste 7 dagen</option>
      <option value="0">alles</option>
    </select>
  </div>
  <div class="stats" id="stats"></div>
  <div class="stats" id="status"></div>
  <details class="logs">
    <summary>Processtatus en logregels (wat anders in de terminal staat)</summary>
    <div id="logs">Laden…</div>
  </details>
</header>

<main>
  <div class="list" id="list"><div class="empty">Laden…</div></div>
  <div class="detail" id="detail"><div class="empty">Kies een resultaat om het volledige scherm terug te zien.</div></div>
</main>

<script>
const $ = (id) => document.getElementById(id);
let selected = null;

function esc(s) {
  return (s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// De server markeert treffers met << >>; die zetten we pas na het escapen om,
// zodat OCR-tekst nooit als HTML wordt uitgevoerd.
function highlight(s) {
  return esc(s).replace(/&lt;&lt;/g, "<mark>").replace(/&gt;&gt;/g, "</mark>");
}

// Welke laag deze tekst leverde: de accessibility-boom, OCR, of geen van
// beide (dan zie je het beeld).
function bron(h) {
  if (h.kind === "image") return "beeld";
  return h.source === "uia" ? "uia" : "ocr";
}

function tijd(ts) {
  const d = new Date(ts * 1000);
  const vandaag = new Date().toDateString() === d.toDateString();
  return vandaag
    ? d.toLocaleTimeString("nl-NL", { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleString("nl-NL", { day: "numeric", month: "short",
                                  hour: "2-digit", minute: "2-digit" });
}

function bereik() {
  const secs = Number($("range").value);
  return secs === 0 ? null : Math.floor(Date.now() / 1000) - secs;
}

function params() {
  const p = new URLSearchParams();
  if ($("q").value.trim()) p.set("q", $("q").value.trim());
  if ($("app").value) p.set("app", $("app").value);
  if ($("kind").value) p.set("kind", $("kind").value);
  const from = bereik();
  if (from !== null) p.set("from", from);
  p.set("limit", "80");
  return p;
}

async function zoek() {
  const res = await fetch("/api/search?" + params());
  const data = await res.json();
  const list = $("list");

  if (!data.hits || data.hits.length === 0) {
    list.innerHTML = '<div class="empty">Niets gevonden in dit tijdvak.</div>';
    return;
  }

  list.innerHTML = data.hits.map((h) => `
    <div class="hit" data-id="${h.id}">
      <div class="meta">
        <span class="app">${esc(h.app)}</span>
        <span class="title">${esc(h.title)}</span>
        <span class="kind">${bron(h)}</span>
        <span>${tijd(h.ts)}</span>
      </div>
      <div class="snippet">${highlight(h.snippet)}</div>
    </div>`).join("");

  for (const el of list.querySelectorAll(".hit")) {
    el.onclick = () => toon(Number(el.dataset.id), el);
  }
}

async function toon(id, el) {
  document.querySelectorAll(".hit.sel").forEach((n) => n.classList.remove("sel"));
  if (el) el.classList.add("sel");
  selected = id;

  const res = await fetch("/api/capture/" + id);
  if (!res.ok) return;
  const c = await res.json();

  const beeld = c.has_frame ? `<img src="/api/frame/${id}" alt="schermafbeelding">` : "";
  const reden = c.fallback_reason
    ? `<div class="note">Beeld bewaard omdat geen enkele tekstbron hier iets
       bruikbaars gaf: ${esc(c.fallback_reason)}.</div>`
    : `<div class="note">Tekst gelezen uit ${c.source === "uia"
        ? "de accessibility-boom van de app — exact, geen OCR nodig"
        : "de pixels via OCR"}.</div>`;
  const tekst = c.text && c.text.trim()
    ? `<pre>${esc(c.text)}</pre>`
    : '<div class="empty">Geen tekst herkend op dit scherm.</div>';

  $("detail").innerHTML = `
    <div class="meta">
      <span class="app">${esc(c.app)}</span>
      <span>${tijd(c.ts)}</span>
      <span class="kind">kwaliteit ${c.quality.toFixed(2)}</span>
    </div>
    <div class="title" style="margin-bottom:10px">${esc(c.title)}</div>
    ${reden}${beeld}${tekst}`;
}

async function statistieken() {
  const p = new URLSearchParams();
  const from = bereik();
  if (from !== null) p.set("from", from);
  const res = await fetch("/api/stats?" + p);
  const { stats, frame_bytes } = await res.json();

  const ruis = (stats.skipped || []).reduce((a, [, n]) => a + n, 0);
  const mb = (frame_bytes / 1048576).toFixed(1);
  $("stats").innerHTML = `
    <span><b>${stats.captures}</b> vastgelegd</span>
    <span><b>${stats.uia_captures}</b> via uia</span>
    <span><b>${stats.ocr_captures}</b> via ocr</span>
    <span><b>${stats.image_captures}</b> beeld-fallback</span>
    <span><b>${ruis}</b> ruis weggefilterd</span>
    <span><b>${stats.segments}</b> vensters</span>
    <span><b>${mb} MB</b> aan frames</span>`;
}

async function appLijst() {
  const res = await fetch("/api/apps");
  const { apps } = await res.json();
  const sel = $("app");
  sel.innerHTML = '<option value="">alle apps</option>' +
    apps.map((a) => `<option value="${esc(a)}">${esc(a)}</option>`).join("");
}

// Dezelfde regels als in de terminal bij het opstarten, maar dan als staat.
async function statusbalk() {
  let s;
  try {
    const res = await fetch("/api/status");
    if (!res.ok) return;
    s = await res.json();
  } catch {
    return;
  }
  const delen = [`<span><b>v${esc(s.version)}</b></span>`];
  delen.push(s.recording
    ? "<span><b>●</b> opname actief</span>"
    : "<span>alleen webinterface</span>");
  if (s.monitors) delen.push(`<span>schermen: ${esc(s.monitors)}</span>`);
  delen.push(`<span>uia: ${s.uia_active ? "actief" : s.uia_enabled ? "startte niet" : "uit"}</span>`);
  delen.push(`<span>ocr: ${s.ocr_language ? esc(s.ocr_language) : s.ocr_enabled ? "startte niet" : "uit"}</span>`);
  if (s.browser_enabled) {
    delen.push(`<span>bridge: ${s.browser_running ? "actief op :" + s.browser_port : "poort bezet"}</span>`);
  }
  if (s.ship_enabled) {
    const laatste = s.ship_last_count != null
      ? ` — laatste batch ${s.ship_last_count}`
      : "";
    delen.push(`<span>shipper: ${s.ship_total} verzonden${laatste}</span>`);
  } else {
    delen.push("<span>alles blijft lokaal</span>");
  }
  delen.push(`<span>events: ${s.foreground_events == null ? "…" : s.foreground_events ? "actief" : "alleen interval"}</span>`);
  delen.push(`<span>autostart: ${s.autostart ? "aan" : "uit"}</span>`);
  delen.push(`<span>gestart: ${tijd(s.started_at)}</span>`);
  $("status").innerHTML = delen.join("");
}

async function logregels() {
  let logs;
  try {
    const res = await fetch("/api/logs");
    if (!res.ok) return;
    ({ logs } = await res.json());
  } catch {
    return;
  }
  if (!logs || logs.length === 0) {
    $("logs").textContent = "Nog geen regels.";
    return;
  }
  $("logs").innerHTML = logs.map((l) =>
    `<div class="log-${esc(l.level)}">${esc(l.ts)} ${esc(l.level)} ${esc(l.text)}</div>`
  ).join("");
  const box = $("logs");
  box.scrollTop = box.scrollHeight;
}

let timer;
function ververs() {
  clearTimeout(timer);
  timer = setTimeout(() => { zoek(); statistieken(); }, 200);
}

$("q").oninput = ververs;
$("app").onchange = ververs;
$("kind").onchange = ververs;
$("range").onchange = () => {
  $("range-label").textContent = $("range").selectedOptions[0].textContent;
  ververs();
};

appLijst();
zoek();
statistieken();
statusbalk();
logregels();
// Terwijl de opname draait komt er vanzelf nieuw materiaal bij.
setInterval(() => { if (!$("q").value.trim()) { zoek(); statistieken(); } statusbalk(); }, 15000);
// Logregels alleen verversen als het blok openstaat.
setInterval(() => {
  if (document.querySelector("details.logs").open) logregels();
}, 5000);
</script>
</body>
</html>
"##;
