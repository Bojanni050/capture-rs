// Draait op elke pagina. Meldt hostnaam + aanwezigheid van een wachtwoordveld
// aan de background service worker, die het naar de lokale Capture-opname
// doorstuurt. Gedebounced en gededupliceerd zodat een drukke pagina niet bij
// elke DOM-mutatie de bridge bestookt.
(function () {
  const HEARTBEAT_MS = 5000;
  let lastReportedKey = null;
  let debounceTimer = null;

  function hasPasswordField() {
    return document.querySelector('input[type="password"]') !== null;
  }

  function state() {
    return {
      type: "capture-status",
      hostname: location.hostname,
      hasPasswordField: hasPasswordField(),
      focused: document.hasFocus(),
    };
  }

  function send(s) {
    chrome.runtime.sendMessage(s).catch(() => {});
  }

  function reportNow() {
    const s = state();
    const key = `${s.hostname}|${s.hasPasswordField}|${s.focused}`;
    if (key === lastReportedKey) return;
    lastReportedKey = key;
    send(s);
  }

  function scheduleReport() {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(reportNow, 300);
  }

  reportNow();

  new MutationObserver(scheduleReport).observe(document.documentElement, {
    childList: true,
    subtree: true,
  });

  document.addEventListener("visibilitychange", reportNow);
  window.addEventListener("focus", reportNow);
  window.addEventListener("blur", reportNow);

  // Zonder deze hartslag zou de opname een pagina waar je een minuut op blijft
  // als "verlopen" zien (de dedup hierboven zwijgt bij ongewijzigde status) en
  // dan geen domeincheck meer doen.
  setInterval(() => {
    if (document.hasFocus() && !document.hidden) send(state());
  }, HEARTBEAT_MS);
})();
