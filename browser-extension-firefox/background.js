// Firefox-variant van background.js: geen service worker maar een
// event-page-script, en de belofte-API via `browser.*` (met een
// terugval op `chrome.*` zodat hetzelfde bestand ook in Chromium
// accumulaties kan draaien).

const BRIDGE_URL = "http://127.0.0.1:8765/browser-status";

// Firefox MV3 gebruikt geen service workers: this-script draait als
// background script met callbacks via de `browser`-namespace. De
// WebExtension-polyfill is hier niet nodig; alleen beloften waar
// nodig handmatig.

// Firefox MV3 gebruikt geen service workers: dit script draait als
// background script. De API heet hier `browser.*` (met terugval op
// `chrome.*`).
const api = typeof browser !== "undefined" ? browser : chrome;
let activeTabId = null;

async function initActiveTab() {
  try {
    const [tab] = await api.tabs.query({ active: true, lastFocusedWindow: true });
    if (tab) activeTabId = tab.id;
  } catch {}
}
initActiveTab();

api.tabs.onActivated.addListener(({ tabId }) => {
  activeTabId = tabId;
});

api.windows.onFocusChanged.addListener(async (windowId) => {
  if (windowId === api.windows.WINDOW_ID_NONE) return;
  try {
    const [tab] = await api.tabs.query({ active: true, windowId });
    if (tab) activeTabId = tab.id;
  } catch {}
});

api.runtime.onMessage.addListener((message, sender) => {
  if (message?.type !== "capture-status") return;
  const tabId = sender.tab?.id;
  if (tabId === undefined) return;
  if (tabId !== activeTabId && !message.focused) return;
  fetch(BRIDGE_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      hostname: message.hostname,
      hasPasswordField: message.hasPasswordField,
    }),
  }).catch(() => {}); // opname draait niet — niets aan te doen
});
