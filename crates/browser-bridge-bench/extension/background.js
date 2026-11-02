// TEST-ONLY фон стендового перехватчика (сессия 08 моста). Не настоящий экстеншен: в сессии 09
// его заменит настоящий одним параметром. Один файл для Chromium (service worker) и
// Firefox (background.scripts); список lib-файлов дублирует manifest.template.json.
"use strict";
if (typeof importScripts === "function") {
  importScripts("config.js", "lib/frame.js", "lib/base64.js", "lib/session-driver.js");
}
const api = typeof browser !== "undefined" ? browser : chrome;
const lib = globalThis.__fpBench;
const config = globalThis.BENCH_CONFIG || {};
if (!config.hostName || !config.reportOrigin) {
  throw new Error("BENCH_CONFIG is not filled in (config.js must be generated from config.template.js)");
}

const startedAt = Date.now();
const SESSION_KEY_NONCE = Date.now().toString(36).slice(-6); // ключ сеанса: base64url-алфавит, ≤ 64 байт
const sessions = new Map(); // "tabId:frameId" -> {driver, tabId, closeReason?}

// Журнал в стенд: POST {source:"extension", run, line}. Ошибки отправки не глотаем — в консоль фона.
function reportLine(run, line) {
  const text = "+" + ((Date.now() - startedAt) / 1000).toFixed(2) + "s " + line;
  fetch(config.reportOrigin + "/bench/log", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ source: "extension", run, line: text }),
  }).catch((error) => console.error("bench log post failed: " + error + " :: " + text));
}

// Заголовок — только печатные символы и не длиннее лимита протокола (1024 байта UTF-8).
function sanitizeTitle(title) {
  return String(title || "").replace(/[\u0000-\u001f\u007f]/g, " ").slice(0, 200);
}

function domainOf(url) {
  try { return new URL(url).hostname; } catch (error) { return "unknown"; }
}

function createDriverFor(sender, run) {
  const tabId = sender.tab ? sender.tab.id : -1;
  const frameId = sender.frameId || 0;
  const tabUrl = (sender.tab && sender.tab.url) || sender.url || "";
  return lib.createSessionDriver({
    hostName: config.hostName,
    sessionKey: "tab-" + tabId + "-f" + frameId + "-" + SESSION_KEY_NONCE,
    page: { title: sanitizeTitle(sender.tab && sender.tab.title), domain: domainOf(tabUrl) },
    frameSequencer: lib.createFrameSequencer(),
    connectNative: (name) => api.runtime.connectNative(name),
    createWebSocket: (url) => new WebSocket(url),
    log: (line) => reportLine(run, line),
  });
}

function handleHookMessage(port, state, payload) {
  const run = payload.run || "unknown";
  if (payload.kind === "log") { reportLine(run, payload.line); return; }
  if (!state.driver) state.driver = createDriverFor(port.sender, run);
  if (payload.kind === "message") {
    if (payload.message.type === "source_buffer_added") reportLine(run, "source buffer added " + payload.message.mime_type);
    state.driver.push({ kind: "message", message: payload.message });
  } else if (payload.kind === "append") {
    state.driver.push({ kind: "append", generation: payload.generation, source_buffer: payload.source_buffer, bytes: lib.base64ToBytes(payload.base64) });
  }
}

// Порт закрылся: вкладка исчезла -> "closed", иначе фрейм ушёл на другую страницу -> "navigated_away".
async function closeAfterDisconnect(state, tabId) {
  if (!state.driver) return;
  let reason = "navigated_away";
  if (!state.tabRemoved) {
    try { await api.tabs.get(tabId); } catch (error) { reason = "closed"; }
  } else {
    reason = "closed";
  }
  state.driver.close(reason);
}

api.runtime.onConnect.addListener((port) => {
  if (port.name !== "bench-hook") return;
  const tabId = port.sender && port.sender.tab ? port.sender.tab.id : -1;
  const key = tabId + ":" + (port.sender.frameId || 0);
  const state = { driver: null, tabRemoved: false };
  sessions.set(key, state);
  port.onMessage.addListener((payload) => {
    try { handleHookMessage(port, state, payload); } catch (error) { reportLine(payload.run || "unknown", "ERROR background: " + error); }
  });
  port.onDisconnect.addListener(() => {
    sessions.delete(key);
    closeAfterDisconnect(state, tabId).catch((error) => console.error("close failed: " + error));
  });
});

// Закрытие вкладки: отметить, чтобы причина была "closed" без гонки с tabs.get.
api.tabs.onRemoved.addListener((tabId) => {
  for (const [key, state] of sessions) if (key.startsWith(tabId + ":")) state.tabRemoved = true;
});
