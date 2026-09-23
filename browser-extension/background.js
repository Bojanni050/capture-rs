// Stuurt meldingen van de content-script door naar de lokale Capture-opname,
// maar alleen voor het tabblad dat echt actief is: achtergrondtabs melden ook
// (zodat de gegevens vers zijn zodra je erheen wisselt), maar hun updates
// negeren we tenzij ze de aandacht hebben.
const BRIDGE_URL = "http://127.0.0.1:8765/browser-status";

let activeTabId = null;

chrome.tabs.query({ active: true, lastFocusedWindow: true }).then(([tab]) => {
  if (tab) activeTabId = tab.id;
});

chrome.tabs.onActivated.addListener(({ tabId }) => {
  activeTabId = tabId;
});

chrome.windows.onFocusChanged.addListener(async (windowId) => {
  if (windowId === chrome.windows.WINDOW_ID_NONE) return;
  const [tab] = await chrome.tabs.query({ active: true, windowId });
  if (tab) activeTabId = tab.id;
});

chrome.runtime.onMessage.addListener((message, sender) => {
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
