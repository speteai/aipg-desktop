// Quick prompt window: one line, Enter hands the text to the main window's chat.
(() => {
  const T = window.__TAURI__ || {};
  const invoke = (cmd, args) => (T.core ? T.core.invoke(cmd, args) : Promise.resolve());
  const q = document.getElementById("q");
  let focusedAt = 0;
  const hide = () => { q.value = ""; invoke("quick_hide").catch(() => {}); };
  window.addEventListener("focus", () => { focusedAt = Date.now(); q.focus(); q.select(); });
  // clicking elsewhere dismisses it (ignore the blur that can accompany showing)
  window.addEventListener("blur", () => { if (focusedAt && Date.now() - focusedAt > 300) hide(); });
  q.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.preventDefault(); hide(); }
    if (e.key === "Enter") {
      e.preventDefault();
      const text = q.value.trim();
      if (!text) return;
      q.value = "";
      invoke("quick_submit", { text }).catch(() => {});
    }
  });
  q.focus();
})();
