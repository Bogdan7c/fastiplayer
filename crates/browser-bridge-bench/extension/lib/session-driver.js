// TEST-ONLY. Один сеанс моста для одного фрейма страницы: посредник (native messaging) ->
// open_session -> WebSocket плеера -> attach -> упорядоченная выдача сообщений и рамок.
// Все внешние зависимости (connectNative, WebSocket, таймеры, журнал) приходят параметрами,
// поэтому логика проверяется в node фейками без браузера (tests/session-driver.test.mjs).
//
// Инварианты:
//  * порядок сообщений и рамок сохраняется ровно таким, как их положила страница (очередь FIFO);
//  * пока не получен `attached`, ничего кроме attach не уходит — всё ждёт в очереди;
//  * buffer_state{intake:"full"} задерживает ТОЛЬКО двоичные рамки; из-за FIFO всё, что стоит за
//    рамкой, тоже ждёт до intake:"accepting";
//  * память очереди ограничена: сверх лимита новые события отбрасываются с громкой записью в журнал.
(function (root) {
  "use strict";
  const DEFAULTS = {
    protocol: { major: 1, minor: 0 },
    softwareVersion: "0.1.0",
    hostAnswerTimeoutMs: 30000,
    attachTimeoutMs: 10000,
    maxQueuedBytes: 64 * 1024 * 1024, // потолок очереди до attach/при intake:full
    logEveryBytes: 1024 * 1024, // строка «bytes sent» раз в МиБ, чтобы не заспамить журнал
    socketHighWaterBytes: 8 * 1024 * 1024, // выше этого bufferedAmount сокета — ждём, не шлём
    socketRetryMs: 20,
    closeLingerMs: 2000,
  };

  function createSessionDriver(deps) {
    const settings = Object.assign({}, DEFAULTS, deps.settings || {});
    const sequencer = deps.frameSequencer;
    const log = deps.log;
    const setTimer = deps.setTimer || setTimeout;
    const clearTimer = deps.clearTimer || clearTimeout;
    const queue = []; // {text} | {frame}; в порядке страницы
    let queuedBytes = 0;
    let overflowLogged = false;
    let startPromise = null;
    let phase = "idle"; // idle -> starting -> attached -> closed | failed
    let intake = "accepting";
    let socket = null;
    let hostPort = null;
    let sentBytes = 0;
    let loggedBytesMark = 0;
    let retryScheduled = false;
    let lastClockState = null;
    const sentTextTypes = []; // для тестов и диагностики

    function fail(reason) {
      if (phase === "closed" || phase === "failed") return;
      phase = "failed";
      log("ERROR " + reason);
      releaseConnections();
    }

    function releaseConnections() {
      try { if (socket) socket.close(); } catch (error) { log("socket close failed: " + error); }
      try { if (hostPort) hostPort.disconnect(); } catch (error) { log("host port disconnect failed: " + error); }
    }

    // ----- посредник: каждый запрос получает ровно один ответ по порядку -----
    function createHostChannel(port) {
      const waiting = [];
      let closedReason = null;
      port.onMessage.addListener((answer) => {
        const waiter = waiting.shift();
        if (waiter) { clearTimer(waiter.timer); waiter.resolve(answer); }
      });
      port.onDisconnect.addListener(() => {
        closedReason = "host port disconnected";
        if (phase === "starting") fail(closedReason); else log(closedReason);
        while (waiting.length) { const waiter = waiting.shift(); clearTimer(waiter.timer); waiter.reject(new Error(closedReason)); }
      });
      return {
        request(message) {
          return new Promise((resolve, reject) => {
            if (closedReason) { reject(new Error(closedReason)); return; }
            const waiter = { resolve, reject, timer: null };
            waiter.timer = setTimer(() => reject(new Error("host did not answer " + message.type)), settings.hostAnswerTimeoutMs);
            waiting.push(waiter);
            port.postMessage(message);
          });
        },
      };
    }

    async function negotiateSession() {
      hostPort = deps.connectNative(deps.hostName);
      const host = createHostChannel(hostPort);
      const hello = await host.request({ type: "hello", protocol: settings.protocol, software_version: settings.softwareVersion, capabilities: [] });
      if (hello.type !== "hello_accepted") throw new Error("hello answered with " + JSON.stringify(hello));
      log("host hello accepted");
      const granted = await host.request({
        type: "open_session",
        request_id: 1,
        session_key: deps.sessionKey,
        page: { title: deps.page.title, domain: deps.page.domain },
        intent: "window",
        hints: { duration: { kind: "unknown" }, position: { kind: "unknown" } },
      });
      if (granted.type !== "session_granted") throw new Error("open_session answered with " + JSON.stringify(granted));
      log("session granted port=" + granted.port + " outcome=" + granted.outcome);
      return granted;
    }

    // ----- WebSocket плеера -----
    function openSocket(granted) {
      return new Promise((resolve, reject) => {
        socket = deps.createWebSocket("ws://127.0.0.1:" + granted.port + "/");
        socket.binaryType = "arraybuffer";
        let attachTimer = null;
        socket.onopen = () => {
          send(JSON.stringify({ type: "attach", session_key: granted.session_key, token: granted.token }), "attach");
          attachTimer = setTimer(() => reject(new Error("player did not answer attach")), settings.attachTimeoutMs);
        };
        socket.onmessage = (event) => {
          if (handlePlayerMessage(event.data)) { clearTimer(attachTimer); resolve(); }
        };
        socket.onerror = () => log("socket error");
        socket.onclose = (event) => {
          clearTimer(attachTimer);
          log("socket closed code=" + event.code);
          if (phase === "starting") reject(new Error("socket closed before attach, code=" + event.code));
          else if (phase === "attached") phase = "closed";
        };
      });
    }

    function send(payload, typeForLog) {
      socket.send(payload);
      if (typeForLog) sentTextTypes.push(typeForLog);
    }

    // true, когда пришёл `attached`. Остальные типы просто фиксируем в журнале.
    function handlePlayerMessage(raw) {
      if (typeof raw !== "string") { log("unexpected binary from player"); return false; }
      let message;
      try { message = JSON.parse(raw); } catch (error) { log("bad JSON from player: " + error); return false; }
      switch (message.type) {
        case "attached":
          log("attached");
          return true;
        case "buffer_state":
          if (message.intake !== intake) log("player intake " + message.intake);
          intake = message.intake;
          if (intake === "accepting") pump();
          return false;
        case "clock":
          if (message.state !== lastClockState) log("player clock " + message.state + " at " + message.position_us);
          lastClockState = message.state;
          return false;
        default:
          log("player message " + raw.slice(0, 200)); // stream_rejected, play, pause, seek ...
          return false;
      }
    }

    // ----- выдача очереди -----
    function pump() {
      while (phase === "attached" && queue.length > 0 && socket && socket.readyState === 1) {
        const item = queue[0];
        if (item.frame) {
          if (intake === "full") return; // продолжим по buffer_state{accepting}
          if (socket.bufferedAmount > settings.socketHighWaterBytes) { scheduleRetry(); return; }
          send(item.frame);
          sentBytes += item.frame.byteLength;
          if (sentBytes - loggedBytesMark >= settings.logEveryBytes) { loggedBytesMark = sentBytes; log("bytes sent " + sentBytes); }
        } else {
          send(item.text, JSON.parse(item.text).type);
        }
        queue.shift();
        queuedBytes -= item.size;
      }
    }

    function scheduleRetry() {
      if (retryScheduled) return;
      retryScheduled = true;
      setTimer(() => { retryScheduled = false; pump(); }, settings.socketRetryMs);
    }

    function enqueue(item) {
      if (queuedBytes + item.size > settings.maxQueuedBytes) {
        if (!overflowLogged) { overflowLogged = true; log("ERROR queue cap " + settings.maxQueuedBytes + " bytes exceeded, dropping events"); }
        return;
      }
      queue.push(item);
      queuedBytes += item.size;
    }

    async function start() {
      phase = "starting";
      try {
        const granted = await negotiateSession();
        await openSocket(granted);
        if (phase !== "starting") return; // закрыли, пока шёл handshake
        phase = "attached";
        pump();
      } catch (error) {
        fail(String((error && error.message) || error));
      }
    }

    return {
      // item: {kind:"message", message} | {kind:"append", generation, source_buffer, bytes}
      push(item) {
        if (phase === "closed" || phase === "failed") return;
        if (item.kind === "message") {
          const text = JSON.stringify(item.message);
          enqueue({ text, size: text.length });
        } else if (item.kind === "append") {
          for (const frame of sequencer.framesFor(item.generation, item.source_buffer, item.bytes)) enqueue({ frame, size: frame.byteLength });
        }
        if (!startPromise) startPromise = start();
        pump();
      },
      // reason: "closed" | "navigated_away" | "browser_closing" (значения tab_gone протокола).
      close(reason) {
        if (phase === "closed" || phase === "failed") return;
        const wasAttached = phase === "attached";
        phase = "closed";
        if (wasAttached && socket && socket.readyState === 1) {
          send(JSON.stringify({ type: "tab_gone", reason }), "tab_gone");
          log("sent tab_gone " + reason);
          setTimer(releaseConnections, settings.closeLingerMs);
        } else {
          releaseConnections();
        }
      },
      // Для тестов: ждёт окончания handshake (успешного или нет).
      whenStarted() { return startPromise || Promise.resolve(); },
      state() { return { phase, intake, queuedItems: queue.length, queuedBytes, sentBytes, sentTextTypes: sentTextTypes.slice() }; },
    };
  }

  const api = { createSessionDriver, DEFAULTS };
  root.__fpBench = Object.assign(root.__fpBench || {}, api);
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis);
