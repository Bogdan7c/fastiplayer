// TEST-ONLY перехватчик стенда моста (сессия 08), MAIN-мир страницы. Заменяется настоящим
// экстеншеном в сессии 09. Грубо оборачивает MSE: страница играет САМА (тени нет), мы лишь
// копируем то, что она скармливает MediaSource, и отдаём ISOLATED-мосту через window.postMessage.
// Правила: всегда вызываем оригинал, сообщаем только об УСПЕШНЫХ вызовах (иначе порядок событий
// разойдётся с реальным буфером), и хук сам никогда не бросает исключений в страницу.
(function () {
  "use strict";
  const lib = globalThis.__fpBench;
  if (!lib || !lib.resolveActivation || typeof MediaSource === "undefined") return;
  if (!lib.resolveActivation(window).handoff) return; // без handoff=1 страница не трогается вообще
  if (globalThis.__fpBenchHookInstalled) return;
  globalThis.__fpBenchHookInstalled = true;

  const post = window.postMessage.bind(window);
  let bridgeReady = false;
  const waitingForBridge = []; // события до рукопожатия с мостом (гонка порядка запуска скриптов)

  function send(payload, transfer) {
    const envelope = Object.assign({ channel: lib.HOOK_CHANNEL, from: "hook" }, payload);
    if (!bridgeReady) { waitingForBridge.push([envelope, transfer]); return; }
    post(envelope, "*", transfer || []);
  }

  // Рукопожатие: мост шлёт "bridge-ready" при старте и в ответ на наш "hook-hello".
  window.addEventListener("message", (event) => {
    const data = event.data;
    if (event.source !== window || !data || data.channel !== lib.HOOK_CHANNEL || data.from !== "bridge" || data.kind !== "bridge-ready") return;
    if (bridgeReady) return;
    bridgeReady = true;
    for (const [envelope, transfer] of waitingForBridge.splice(0)) post(envelope, "*", transfer || []);
  });

  const tracker = lib.createGenerationTracker({
    emit(item) {
      // Байты уходят мосту СТРОКОЙ base64, а не ArrayBuffer: в Firefox ISOLATED-мир не может читать
      // ArrayBuffer страницы ("Permission denied to access property constructor"), строка — примитив.
      if (item.kind === "append") send({ kind: "append", generation: item.generation, source_buffer: item.source_buffer, base64: lib.bytesToBase64(item.bytes) });
      else send(item);
    },
  });

  function safely(what, action) {
    try { action(); } catch (error) { try { send({ kind: "log", line: "hook error in " + what + ": " + error }); } catch (ignored) { /* хуку нельзя ронять страницу */ } }
  }

  // Копия ровно того диапазона, который видит appendBuffer: для view — byteOffset/byteLength, а не весь buffer.
  function copyBytes(data) {
    if (data instanceof ArrayBuffer) return new Uint8Array(data.slice(0));
    if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength).slice();
    return null;
  }

  function wrapMethod(prototype, name, buildWrapper) {
    const original = prototype[name];
    if (typeof original !== "function") { send({ kind: "log", line: "hook skipped: " + name + " missing" }); return; }
    prototype[name] = buildWrapper(original);
  }

  // Сеттер оборачиваем, читая итоговое значение геттером: браузер сам нормализует число.
  function wrapSetter(prototype, name, onChanged) {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    if (!descriptor || !descriptor.set || !descriptor.get) { send({ kind: "log", line: "hook skipped: " + name + " setter missing" }); return; }
    Object.defineProperty(prototype, name, Object.assign({}, descriptor, {
      set(value) {
        descriptor.set.call(this, value);
        safely(name, () => onChanged(this, descriptor.get.call(this)));
      },
    }));
  }

  wrapMethod(MediaSource.prototype, "addSourceBuffer", (original) => function (mime) {
    const sourceBuffer = original.apply(this, arguments);
    safely("addSourceBuffer", () => tracker.noteSourceBufferAdded(this, sourceBuffer, mime));
    return sourceBuffer;
  });
  wrapMethod(MediaSource.prototype, "endOfStream", (original) => function (errorArgument) {
    const result = original.apply(this, arguments);
    safely("endOfStream", () => tracker.noteEndOfStream(this, errorArgument));
    return result;
  });
  wrapSetter(MediaSource.prototype, "duration", (mediaSource, seconds) => tracker.noteDuration(mediaSource, seconds));

  if (typeof SourceBuffer !== "undefined") {
    wrapMethod(SourceBuffer.prototype, "appendBuffer", (original) => function (data) {
      let copy = null;
      safely("appendBuffer copy", () => { copy = copyBytes(data); }); // копия ДО оригинала
      const result = original.apply(this, arguments);
      if (copy) safely("appendBuffer", () => tracker.noteAppend(this, copy));
      return result;
    });
    wrapMethod(SourceBuffer.prototype, "changeType", (original) => function (mime) {
      const result = original.apply(this, arguments);
      safely("changeType", () => tracker.noteChangeType(this, mime));
      return result;
    });
    wrapMethod(SourceBuffer.prototype, "remove", (original) => function (start, end) {
      const result = original.apply(this, arguments);
      safely("remove", () => tracker.noteRemove(this, start, end));
      return result;
    });
    wrapMethod(SourceBuffer.prototype, "abort", (original) => function () {
      const result = original.apply(this, arguments);
      safely("abort", () => tracker.noteAbort(this));
      return result;
    });
    wrapSetter(SourceBuffer.prototype, "timestampOffset", (sourceBuffer, seconds) => tracker.noteTimestampOffset(sourceBuffer, seconds));
  }

  post({ channel: lib.HOOK_CHANNEL, from: "hook", kind: "hook-hello" }, "*");
  send({ kind: "log", line: "hook installed" });
})();
