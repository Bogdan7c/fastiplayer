// Прогон НАСТОЯЩИХ page-hook.js и lib-файлов в vm-контексте с фейковыми MediaSource/SourceBuffer:
// проверяем то, что видит мост — копию ровно нужного диапазона, порядок, неломание страницы.
import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { readFileSync } from "node:fs";
import path from "node:path";
import { base64Lib, extensionDir } from "./support.mjs";

function runHook({ search }) {
  const posted = [];
  const listeners = [];
  const sandbox = {
    location: { search },
    postMessage: (message, targetOrigin, transfer) => { posted.push({ message, transfer }); },
    addEventListener: (type, listener) => { if (type === "message") listeners.push(listener); },
  };
  sandbox.top = sandbox;
  sandbox.window = sandbox;
  sandbox.globalThis = sandbox;
  class FakeSourceBuffer {
    constructor() { this.received = []; this._offset = 0; }
    appendBuffer(data) { if (this.failNext) { this.failNext = false; throw new Error("QuotaExceededError"); } this.received.push(data); }
    changeType() {}
    remove() {}
    abort() {}
    get timestampOffset() { return this._offset; }
    set timestampOffset(value) { this._offset = value; }
  }
  class FakeMediaSource {
    constructor() { this._duration = NaN; }
    addSourceBuffer() { return new FakeSourceBuffer(); }
    endOfStream() {}
    get duration() { return this._duration; }
    set duration(value) { this._duration = value; }
  }
  Object.assign(sandbox, { btoa, MediaSource: FakeMediaSource, SourceBuffer: FakeSourceBuffer, URLSearchParams, Uint8Array, ArrayBuffer, Number, Object, console });
  vm.createContext(sandbox);
  for (const file of ["lib/activation.js", "lib/base64.js", "lib/generation-tracker.js", "page-hook.js"]) {
    vm.runInContext(readFileSync(path.join(extensionDir, file), "utf8"), sandbox, { filename: file });
  }
  const bridgeReady = () => listeners.forEach((listener) =>
    listener({ source: vm.runInContext("window", sandbox), data: { channel: "fastiplayer-bench-hook-v1", from: "bridge", kind: "bridge-ready" } }));
  const hookMessages = () => posted.map((entry) => entry.message).filter((message) => message.from === "hook");
  return { sandbox, bridgeReady, hookMessages, posted };
}

test("without handoff=1 the page is not touched at all", () => {
  const { sandbox, hookMessages } = runHook({ search: "?run=abc" });
  assert.equal(sandbox.__fpBenchHookInstalled, undefined);
  assert.equal(hookMessages().length, 0);
  const sourceBuffer = new sandbox.MediaSource().addSourceBuffer("video/mp4");
  sourceBuffer.appendBuffer(Uint8Array.of(1));
  assert.equal(hookMessages().length, 0);
});

test("events wait for the bridge handshake, then arrive in page order; appendBuffer bytes are an exact copy of the viewed range", () => {
  const { sandbox, bridgeReady, hookMessages } = runHook({ search: "?run=r1&handoff=1" });
  const mediaSource = new sandbox.MediaSource();
  const sourceBuffer = mediaSource.addSourceBuffer('video/mp4; codecs="avc1.64001e"');
  const backing = Uint8Array.of(0, 1, 2, 3, 4, 5, 6, 7);
  sourceBuffer.appendBuffer(backing.subarray(2, 5)); // view со смещением
  backing.fill(99); // страница портит исходный буфер после вызова — копия не должна измениться
  sourceBuffer.appendBuffer(Uint8Array.of(7, 7).buffer); // голый ArrayBuffer
  mediaSource.duration = 30;
  mediaSource.endOfStream();
  assert.equal(hookMessages().filter((message) => message.kind !== "hook-hello").length, 0, "nothing leaks before bridge-ready");
  bridgeReady();
  const sent = hookMessages().filter((message) => message.kind !== "hook-hello");
  assert.deepEqual(sent.map((message) => message.kind === "message" ? message.message.type : message.kind), [
    "log", // hook installed
    "generation_started", "source_buffer_added", "append", "append", "duration_changed", "end_of_stream",
  ]);
  assert.equal(sent[0].line, "hook installed");
  const appends = sent.filter((message) => message.kind === "append");
  assert.deepEqual([...base64Lib.base64ToBytes(appends[0].base64)], [2, 3, 4]);
  assert.deepEqual([...base64Lib.base64ToBytes(appends[1].base64)], [7, 7]);
  assert.deepEqual(sourceBuffer.received.length, 2, "original appendBuffer was still called for the page");
});

test("a failing original appendBuffer rethrows to the page and reports nothing", () => {
  const { sandbox, bridgeReady, hookMessages } = runHook({ search: "?handoff=1" });
  bridgeReady();
  const sourceBuffer = new sandbox.MediaSource().addSourceBuffer("video/mp4");
  sourceBuffer.failNext = true;
  assert.throws(() => sourceBuffer.appendBuffer(Uint8Array.of(1, 2)), /QuotaExceededError/);
  assert.equal(hookMessages().filter((message) => message.kind === "append").length, 0);
});

test("timestampOffset setter keeps working for the page and is reported once set", () => {
  const { sandbox, bridgeReady, hookMessages } = runHook({ search: "?handoff=1" });
  bridgeReady();
  const sourceBuffer = new sandbox.MediaSource().addSourceBuffer("video/mp4");
  sourceBuffer.timestampOffset = 2;
  assert.equal(sourceBuffer.timestampOffset, 2);
  const offsets = hookMessages().filter((message) => message.kind === "message" && message.message.type === "timestamp_offset_changed");
  assert.equal(offsets.length, 1);
  assert.equal(offsets[0].message.offset_us, 2000000);
});
