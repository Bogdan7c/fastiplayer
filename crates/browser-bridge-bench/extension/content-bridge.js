// TEST-ONLY. ISOLATED-мир: принимает события MAIN-хука (window.postMessage) и пересылает фону
// по порту runtime.connect. Порт один на фрейм: его закрытие (уход со страницы / закрытие вкладки)
// фон трактует как tab_gone. Байты приходят уже строкой base64 (кодирует MAIN-хук, см. page-hook.js и lib/base64.js).
(function () {
  "use strict";
  const lib = globalThis.__fpBench;
  const api = typeof browser !== "undefined" ? browser : chrome;
  const activation = lib.resolveActivation(window);
  if (!activation.handoff) return;

  const port = api.runtime.connect({ name: "bench-hook" });
  const run = activation.run || "unknown";
  const forward = (payload) => {
    try { port.postMessage(Object.assign({ run }, payload)); } catch (error) { console.error("bench bridge: port send failed", error); }
  };
  const announceReady = () => window.postMessage({ channel: lib.HOOK_CHANNEL, from: "bridge", kind: "bridge-ready" }, "*");

  window.addEventListener("message", (event) => {
    const data = event.data;
    if (event.source !== window || !data || data.channel !== lib.HOOK_CHANNEL || data.from !== "hook") return;
    if (data.kind === "hook-hello") announceReady();
    else if (data.kind === "log") forward({ kind: "log", line: String(data.line) });
    else if (data.kind === "message") forward({ kind: "message", message: data.message });
    else if (data.kind === "append") {
      forward({ kind: "append", generation: data.generation, source_buffer: data.source_buffer, base64: String(data.base64) });
    }
  });
  announceReady();
})();
