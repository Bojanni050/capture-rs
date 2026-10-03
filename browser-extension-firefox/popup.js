// Popup: toont of de lokale Capture-bridge bereikbaar is en wat de
// laatst gemelde site is. Puur lezen; de POST-route blijft bij de
// background service worker.

const BRIDGE_URL = "http://127.0.0.1:8765/browser-status";

const bridgeDot = document.getElementById("bridge-dot");
const bridgeLabel = document.getElementById("bridge-label");
const siteDot = document.getElementById("site-dot");
const siteLabel = document.getElementById("site-label");
const hint = document.getElementById("hint");

function setBridge(state, text) {
  bridgeDot.className = "dot " + state;
  bridgeLabel.textContent = text;
}

function setSite(state, text) {
  siteDot.className = "dot " + state;
  siteLabel.textContent = text;
}

async function checkBridge() {
  try {
    const res = await fetch(BRIDGE_URL, { method: "GET" });
    if (!res.ok) throw new Error("status " + res.status);
    const data = await res.json();
    if (data.running) {
      setBridge("ok", "Capture draait");
      setSite(
        data.hasPasswordField ? "warn" : "ok",
        data.hasPasswordField
          ? `${data.hostname} — wachtwoordveld gezien, domein uitgesloten`
          : data.hostname,
      );
    } else {
      setBridge("ok", "Capture draait (nog geen site gemeld)");
      setSite("warn", "Wacht op de eerste melding…");
    }
    hint.textContent = " Alles werkt. Sluit de popup om te stoppen.";
  } catch {
    setBridge("err", "Capture niet bereikbaar");
    setSite("err", "Start `capture start` of check de poort");
    hint.textContent =
      " De bridge op 127.0.0.1:8765 reageert niet. Draait de opname?";
  }
}

checkBridge();
