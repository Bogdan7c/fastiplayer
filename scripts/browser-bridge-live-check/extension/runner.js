// Общая часть: проигрывание плана по WebSocket. Работает и в расширении (service worker /
// фоновая страница), и в node (drive-host.mjs), поэтому только стандартные API.
(function (root) {
  "use strict";
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  // Двоичная рамка вида 1: 24 байта LE-заголовка + груз (data_frame.rs).
  function buildFrame(generation, sourceBuffer, sequence, payload) {
    const frame = new Uint8Array(24 + payload.byteLength);
    const view = new DataView(frame.buffer);
    view.setUint8(0, 1);
    view.setUint32(4, generation, true);
    view.setUint16(8, sourceBuffer, true);
    view.setBigUint64(16, BigInt(sequence), true);
    frame.set(payload, 24);
    return frame;
  }

  // opts: url, attach {session_key, token}, plan, media {имя файла: Uint8Array},
  //       WebSocketImpl, log(строка), report(объект), lingerAfterEndMs, maxFullWaitMs
  async function playStream(opts) {
    const { plan, media, log, report } = opts;
    const WS = opts.WebSocketImpl || WebSocket;
    const stats = {
      phase: "connecting", framesSent: 0, textSent: 0, bytesSent: 0,
      clocksReceived: 0, lastClockPositionUs: null, clockState: null,
      bufferStates: 0, intake: "accepting", playerMessageTypes: {},
      fullWaits: 0, closeCode: null, error: null,
    };
    const publish = () => { try { report({ ...stats }); } catch (e) { log("report failed: " + e); } };
    const setPhase = (phase) => { stats.phase = phase; log("phase " + phase); publish(); };

    const socket = new WS(opts.url);
    socket.binaryType = "arraybuffer";
    let attachedResolve, endedResolve, closedResolve;
    const attached = new Promise((r) => { attachedResolve = r; });
    const ended = new Promise((r) => { endedResolve = r; });
    const closed = new Promise((r) => { closedResolve = r; });
    let lastPublish = 0;

    socket.onmessage = (event) => {
      if (typeof event.data !== "string") { log("unexpected binary from player"); return; }
      let message;
      try { message = JSON.parse(event.data); } catch (e) { log("bad JSON from player"); return; }
      stats.playerMessageTypes[message.type] = (stats.playerMessageTypes[message.type] || 0) + 1;
      if (message.type === "attached") { log("player: attached"); attachedResolve(); return; }
      if (message.type === "clock") {
        stats.clocksReceived += 1;
        stats.lastClockPositionUs = message.position_us;
        if (message.state !== stats.clockState) log("player: clock state " + message.state + " at " + message.position_us);
        stats.clockState = message.state;
        if (message.state === "ended") endedResolve();
        if (Date.now() - lastPublish > 1000) { lastPublish = Date.now(); publish(); }
        return;
      }
      if (message.type === "buffer_state") {
        stats.bufferStates += 1;
        stats.intake = message.intake;
        return;
      }
      // Все прочие сообщения плеера (play/pause/seek/stream_rejected...) — в журнал как есть.
      log("player: " + event.data.slice(0, 300));
    };
    socket.onclose = (event) => { stats.closeCode = event.code; log("socket closed code=" + event.code); closedResolve(); };
    socket.onerror = () => { log("socket error"); };

    const timeout = (ms, what) => new Promise((_, reject) => setTimeout(() => reject(new Error("timeout: " + what)), ms));
    try {
      await new Promise((resolve, reject) => {
        socket.onopen = resolve;
        const previousClose = socket.onclose;
        socket.onclose = (e) => { previousClose(e); reject(new Error("closed before open, code=" + e.code)); };
      });
      socket.onclose = (event) => { stats.closeCode = event.code; log("socket closed code=" + event.code); closedResolve(); };
      log("socket open");
      socket.send(JSON.stringify({ type: "attach", ...opts.attach }));
      await Promise.race([attached, timeout(10000, "attached")]);
      setPhase("streaming");

      const maxFullWaitMs = opts.maxFullWaitMs || 30000;
      for (const step of plan.steps) {
        if (stats.closeCode !== null) throw new Error("player closed the socket mid-stream, code=" + stats.closeCode);
        if (step.text) {
          socket.send(JSON.stringify(step.text));
          stats.textSent += 1;
          log("sent " + step.text.type);
          continue;
        }
        const f = step.frame;
        // Обратное давление: пока плеер говорит buffer_state{full} — придерживаем рамки.
        const waitStart = Date.now();
        while (stats.intake === "full" && stats.closeCode === null) {
          if (Date.now() - waitStart > maxFullWaitMs) throw new Error("intake stayed full too long");
          if (Date.now() - waitStart < 50) stats.fullWaits += 1;
          await sleep(100);
        }
        const payload = media[f.file].subarray(f.off, f.off + f.len);
        socket.send(buildFrame(f.generation, f.sb, f.seq, payload));
        stats.framesSent += 1;
        stats.bytesSent += payload.byteLength;
        if (stats.framesSent % 10 === 0) log("sent frames=" + stats.framesSent + " bytes=" + stats.bytesSent + " intake=" + stats.intake);
        // Небольшой темп, чтобы не заливать плеер разом (и не блокировать SW).
        await sleep(opts.frameDelayMs === undefined ? 30 : opts.frameDelayMs);
      }
      setPhase("streamed");
      await Promise.race([ended, closed, timeout(plan.duration_us / 1000 + 40000, "clock ended")]);
      setPhase("ended");
      await sleep(opts.lingerAfterEndMs === undefined ? 2000 : opts.lingerAfterEndMs);
      if (stats.closeCode === null) {
        socket.send(JSON.stringify({ type: "tab_gone", reason: "navigated_away" }));
        log("sent tab_gone");
        await Promise.race([closed, timeout(10000, "close after tab_gone")]);
      }
      setPhase("done");
    } catch (error) {
      stats.error = String(error && error.message || error);
      log("ERROR " + stats.error);
      setPhase("failed");
      try { socket.close(); } catch (e) { /* уже закрыт */ }
    }
    return stats;
  }

  root.BridgeLive = { playStream, buildFrame, sleep };
  if (typeof module !== "undefined") module.exports = root.BridgeLive;
})(typeof globalThis !== "undefined" ? globalThis : self);
