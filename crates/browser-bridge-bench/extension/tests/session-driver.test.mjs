import test from "node:test";
import assert from "node:assert/strict";
import { driverLib, frameLib, protocol, decodeDataFrame, expectValidPageMessage } from "./support.mjs";

const flush = () => new Promise((resolve) => setImmediate(resolve));

// Фейковый посредник: отвечает на hello/open_session как настоящий; режимы отказа задаются.
function fakeHost({ helloReply, openReply } = {}) {
  const received = [];
  const messageListeners = [];
  const disconnectListeners = [];
  const port = {
    onMessage: { addListener: (listener) => messageListeners.push(listener) },
    onDisconnect: { addListener: (listener) => disconnectListeners.push(listener) },
    postMessage(message) {
      received.push(message);
      const reply = message.type === "hello"
        ? helloReply || { type: "hello_accepted", agreed_protocol: { major: 1, minor: 0 }, player: { protocol: { major: 1, minor: 0 }, software_version: "0.2.0", capabilities: [] } }
        : openReply || { type: "session_granted", request_id: 1, session_key: message.session_key, port: 40213, token: "Zml4dHVyZS10b2tlbi1icmlkZ2UtMDEtcHJvdG9jb2w", outcome: "started" };
      queueMicrotask(() => messageListeners.forEach((listener) => listener(reply)));
    },
    disconnect() { port.disconnected = true; },
    disconnected: false,
  };
  return { port, received };
}

// Фейковый WebSocket плеера: на attach отвечает attached; сообщения плеера подаёт тест.
class FakeSocket {
  constructor(url) {
    this.url = url; this.readyState = 0; this.sent = []; this.bufferedAmount = 0; this.closed = false;
    queueMicrotask(() => { this.readyState = 1; this.onopen(); });
  }
  send(payload) {
    this.sent.push(payload);
    if (typeof payload === "string" && JSON.parse(payload).type === "attach") {
      queueMicrotask(() => this.fromPlayer({ type: "attached" }));
    }
  }
  fromPlayer(message) { this.onmessage({ data: JSON.stringify(message) }); }
  close() { this.closed = true; this.readyState = 3; }
}

function newDriver({ host = fakeHost(), settings } = {}) {
  const logs = [];
  const sockets = [];
  const driver = driverLib.createSessionDriver({
    hostName: "io.github.bogdan7c.fastiplayer.bridge",
    sessionKey: "tab-12-f0-abc123",
    page: { title: "Bench page", domain: "127.0.0.1" },
    frameSequencer: frameLib.createFrameSequencer(),
    connectNative: () => host.port,
    createWebSocket: (url) => { const socket = new FakeSocket(url); sockets.push(socket); return socket; },
    log: (line) => logs.push(line),
    settings: Object.assign({ closeLingerMs: 5, attachTimeoutMs: 200, hostAnswerTimeoutMs: 200 }, settings),
  });
  return { driver, host, logs, sockets };
}

const generationStarted = { kind: "message", message: { type: "generation_started", generation: 1 } };
const sourceBufferAdded = { kind: "message", message: { type: "source_buffer_added", generation: 1, source_buffer: 0, mime_type: "video/mp4" } };
const append = (bytes) => ({ kind: "append", generation: 1, source_buffer: 0, bytes: Uint8Array.from(bytes) });

test("handshake: hello, open_session and attach are valid protocol messages, events queued before attach arrive in order", async () => {
  const { driver, host, logs, sockets } = newDriver();
  driver.push(generationStarted); // всё это приходит ДО attach
  driver.push(sourceBufferAdded);
  driver.push(append([1, 2, 3]));
  await driver.whenStarted();
  await flush();

  const hello = protocol.decodeExtensionControl(JSON.stringify(host.received[0]));
  assert.equal(hello.ok, true, JSON.stringify(hello.error));
  const open = protocol.decodeExtensionControl(JSON.stringify(host.received[1]));
  assert.equal(open.ok, true, JSON.stringify(open.error));
  assert.equal(open.value.session_key, "tab-12-f0-abc123");

  const [socket] = sockets;
  assert.equal(socket.url, "ws://127.0.0.1:40213/");
  const [attach, ...rest] = socket.sent;
  assert.equal(expectValidPageMessage(attach).type, "attach");
  assert.deepEqual(rest.slice(0, 2).map((text) => expectValidPageMessage(text).type), ["generation_started", "source_buffer_added"]);
  const frame = decodeDataFrame(rest[2]);
  assert.equal(frame.ok, true);
  assert.deepEqual([...frame.value.payload], [1, 2, 3]);
  assert.ok(logs.includes("host hello accepted"));
  assert.ok(logs.includes("session granted port=40213 outcome=started"));
  assert.ok(logs.includes("attached"));
  assert.equal(driver.state().queuedItems, 0);
});

test("backpressure: after buffer_state full frames wait (and everything behind them); accepting resumes in order", async () => {
  const { driver, sockets } = newDriver();
  driver.push(generationStarted);
  driver.push(sourceBufferAdded);
  await driver.whenStarted();
  await flush();
  const socket = sockets[0];
  socket.sent.length = 0;

  socket.fromPlayer({ type: "buffer_state", generation: 1, ahead_us: 118000000, intake: "full" });
  driver.push(append([1]));
  driver.push({ kind: "message", message: { type: "end_of_stream", generation: 1 } });
  driver.push(append([2]));
  assert.equal(socket.sent.length, 0, "frame held; end_of_stream queued behind it keeps the order");
  assert.equal(driver.state().queuedItems, 3);

  socket.fromPlayer({ type: "buffer_state", generation: 1, ahead_us: 0, intake: "accepting" });
  assert.equal(driver.state().queuedItems, 0);
  assert.equal(decodeDataFrame(socket.sent[0]).value.header.sequence, 0n);
  assert.equal(expectValidPageMessage(socket.sent[1]).type, "end_of_stream");
  assert.equal(decodeDataFrame(socket.sent[2]).value.header.sequence, 1n);
});

test("text messages are not held by intake full when no frame is ahead of them", async () => {
  const { driver, sockets } = newDriver();
  driver.push(generationStarted);
  await driver.whenStarted();
  await flush();
  const socket = sockets[0];
  socket.fromPlayer({ type: "buffer_state", generation: 1, ahead_us: 1, intake: "full" });
  socket.sent.length = 0;
  driver.push(sourceBufferAdded);
  assert.equal(socket.sent.length, 1);
});

test("queue cap: events over the memory limit are dropped with an ERROR log line, not silently", async () => {
  const { driver, logs } = newDriver({ host: fakeHost({ helloReply: { type: "refused", reason: "x" } }), settings: { maxQueuedBytes: 100 } });
  driver.push(append(new Array(60).fill(1))); // рамка 84 байта
  driver.push(append(new Array(60).fill(2))); // не влезает
  assert.equal(driver.state().queuedItems, 1);
  assert.ok(logs.some((line) => line.startsWith("ERROR queue cap")));
});

test("host refusing hello: session fails with ERROR log, no WebSocket is ever opened", async () => {
  const { driver, logs, sockets, host } = newDriver({ host: fakeHost({ helloReply: { type: "refused", reason: "incompatible_version" } }) });
  driver.push(generationStarted);
  await driver.whenStarted();
  assert.equal(driver.state().phase, "failed");
  assert.equal(sockets.length, 0);
  assert.ok(logs.some((line) => line.startsWith("ERROR hello answered with")));
  assert.equal(host.port.disconnected, true);
});

test("close after attach sends a valid tab_gone with the given reason and later pushes are ignored", async () => {
  const { driver, sockets } = newDriver();
  driver.push(generationStarted);
  await driver.whenStarted();
  await flush();
  driver.close("navigated_away");
  const last = expectValidPageMessage(sockets[0].sent.at(-1));
  assert.deepEqual(last, { type: "tab_gone", reason: "navigated_away" });
  const before = sockets[0].sent.length;
  driver.push(append([1]));
  assert.equal(sockets[0].sent.length, before);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(sockets[0].closed, true);
});

test("bytes sent is logged roughly once per configured megabyte, not per frame", async () => {
  const { driver, logs } = newDriver({ settings: { logEveryBytes: 1000 } });
  driver.push(generationStarted);
  await driver.whenStarted();
  await flush();
  for (let index = 0; index < 5; index += 1) driver.push(append(new Array(500).fill(index)));
  const lines = logs.filter((line) => line.startsWith("bytes sent"));
  assert.ok(lines.length >= 2 && lines.length < 5, "throttled: " + JSON.stringify(lines));
});
