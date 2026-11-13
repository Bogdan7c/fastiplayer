// Хук MSE: оригинал первым, только успешные вызовы, байты — ссылкой без копии, никаких исключений
// из хука, сосуществование с чужими подменами в обоих порядках, перевод вызовов в события договора.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { LocalMseEvent } from "../../src/shared/handoff-contract.ts";
import type { ReplayItem } from "../../src/page/journal/media-journal.ts";
import { FakeDomException, createFakePage } from "./support/fake-page.ts";
import { handshake, startHarness, type BridgeHarness } from "./support/bridge-harness.ts";
import { VIDEO_MP4 } from "./support/media-samples.ts";

const MIME = 'video/mp4; codecs="avc1.42E01E"';

function journalOf(harness: BridgeHarness, mediaSource: object): ReplayItem[] {
  const record = harness.bridge.tracker.recordOf(mediaSource);
  assert.ok(record !== null, "media source is tracked");
  return record.journal.replay();
}

function eventsOf(items: readonly ReplayItem[]): LocalMseEvent[] {
  return items.flatMap((item) => (item.kind === "mse" ? [item.event] : []));
}

function bytesOf(items: readonly ReplayItem[]): Uint8Array[] {
  return items.flatMap((item) => (item.kind === "bytes" ? [item.view] : []));
}

test("appendBuffer: the original runs first, the journal keeps a reference to the page's bytes (no copy)", () => {
  const page = createFakePage();
  // «Оригинал» (то, что лежит на прототипе до нас) запоминает, что видел журнал в момент вызова.
  let harness: BridgeHarness | null = null;
  let trackedMediaSource: object | null = null;
  let bytesSeenInsideOriginal = -1;
  const browserAppend = page.FakeSourceBuffer.prototype.appendBuffer;
  page.FakeSourceBuffer.prototype.appendBuffer = function (this: unknown, source: ArrayBuffer | ArrayBufferView) {
    if (harness !== null && trackedMediaSource !== null) bytesSeenInsideOriginal = bytesOf(journalOf(harness, trackedMediaSource)).length;
    return Reflect.apply(browserAppend, this, [source]);
  };
  harness = startHarness({ page });
  const mediaSource = page.attachMediaSource(page.addVideo());
  trackedMediaSource = mediaSource;
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  // Байты страницы лежат посреди большего буфера: вид со смещением 100.
  const file = VIDEO_MP4.bytes();
  const pageBuffer = new ArrayBuffer(file.length + 300);
  new Uint8Array(pageBuffer, 100, file.length).set(file);
  sourceBuffer.appendBuffer(new Uint8Array(pageBuffer, 100, file.length));
  assert.equal(bytesSeenInsideOriginal, 0, "observation happens only after the original returned");

  const [kept] = bytesOf(journalOf(harness, mediaSource));
  assert.ok(kept !== undefined);
  assert.equal(kept.buffer, pageBuffer, "same memory as the page's buffer");
  assert.equal(kept.byteOffset, 100);
  assert.equal(kept.byteLength, file.length);
  assert.deepEqual(kept, sourceBuffer.appended[0]?.bytes, "exactly the bytes the browser took");
});

test("appendBuffer accepts ArrayBuffer, typed views and DataView with exact ranges", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  const file = VIDEO_MP4.bytes();
  const whole = file.slice(0, 733).buffer;
  const sharedBig = file.slice().buffer;
  sourceBuffer.appendBuffer(whole);
  sourceBuffer.appendBuffer(new DataView(sharedBig, 733, 2000));
  sourceBuffer.appendBuffer(new Uint16Array(sharedBig, 2734, 1490)); // 2980 байт, смещение чётное
  const kept = bytesOf(journalOf(harness, mediaSource));
  assert.deepEqual(kept, [file.subarray(0, 733), file.subarray(733, 2733), file.subarray(2734, 2734 + 2980)]);
  assert.deepEqual(
    kept.map((view) => view.buffer),
    [whole, sharedBig, sharedBig],
    "no copies: every view points into the page's own buffers",
  );
});

test("a refused call is not recorded and the page gets exactly its own exception", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  const quota = new FakeDomException("QuotaExceededError");
  sourceBuffer.failNextAppend = quota;
  assert.throws(() => sourceBuffer.appendBuffer(VIDEO_MP4.bytes()), (error) => error === quota);
  assert.deepEqual(bytesOf(journalOf(harness, mediaSource)), []);

  // Отказ addSourceBuffer: SourceBuffer не появился, событий нет.
  assert.throws(() => mediaSource.addSourceBuffer("unsupported/codec"), (error) => error instanceof FakeDomException && error.name === "NotSupportedError");
  // Отказ сеттера duration (NaN): событие не пишется.
  assert.throws(() => {
    mediaSource.duration = Number.NaN;
  }, TypeError);
  assert.deepEqual(eventsOf(journalOf(harness, mediaSource)), [{ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MIME }]);
});

test("before a handoff nothing is posted to the port even with a connected channel", async () => {
  const harness = startHarness();
  const { page } = harness;
  const isolated = handshake(page);
  try {
    const mediaSource = page.attachMediaSource(page.addVideo());
    const sourceBuffer = mediaSource.addSourceBuffer(MIME);
    mediaSource.duration = 10;
    sourceBuffer.appendBuffer(VIDEO_MP4.bytes());
    sourceBuffer.timestampOffset = 2;
    await isolated.settle();
    assert.equal(page.postedToPorts.length, 0);
    assert.equal(isolated.received.length, 0);
  } finally {
    isolated.close();
  }
});

test("MSE calls become contract events in call order (pending duration, remove to Infinity, endOfStream with error ignored)", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  mediaSource.duration = 12.5; // до первого SourceBuffer
  const video = mediaSource.addSourceBuffer(MIME);
  const audio = mediaSource.addSourceBuffer('audio/mp4; codecs="mp4a.40.2"');
  video.timestampOffset = 1.2345678; // фейк-браузер нормализует до миллисекунды
  video.remove(0, Number.POSITIVE_INFINITY);
  audio.remove(1.5, 3);
  video.abort();
  video.changeType('video/mp4; codecs="avc1.640028"');
  mediaSource.duration = Number.POSITIVE_INFINITY;
  mediaSource.endOfStream("network"); // сбой страницы, не конец потока
  mediaSource.testOpen(); // страница вернула MediaSource в работу
  mediaSource.endOfStream();

  const key = "ms-1";
  assert.deepEqual(eventsOf(journalOf(harness, mediaSource)), [
    { kind: "source_buffer_added", media_source: key, source_buffer: 0, mime_type: MIME },
    { kind: "duration_changed", media_source: key, duration: { kind: "finite", duration_us: 12_500_000 } },
    { kind: "source_buffer_added", media_source: key, source_buffer: 1, mime_type: 'audio/mp4; codecs="mp4a.40.2"' },
    { kind: "timestamp_offset_changed", media_source: key, source_buffer: 0, offset_us: 1_235_000 },
    { kind: "source_buffer_removed", media_source: key, source_buffer: 0, start_us: 0, end: { kind: "end_of_timeline" } },
    { kind: "source_buffer_removed", media_source: key, source_buffer: 1, start_us: 1_500_000, end: { kind: "at", time_us: 3_000_000 } },
    { kind: "source_buffer_aborted", media_source: key, source_buffer: 0 },
    { kind: "source_buffer_type_changed", media_source: key, source_buffer: 0, mime_type: 'video/mp4; codecs="avc1.640028"' },
    { kind: "duration_changed", media_source: key, duration: { kind: "live" } },
    // endOfStream("network") не пересказан.
    { kind: "end_of_stream", media_source: key },
  ]);
});

test("source buffers past 8 and protocol-unfriendly MIME are not tracked; media sources get ms-1, ms-2 keys", () => {
  const harness = startHarness();
  const { page } = harness;
  const first = page.attachMediaSource(page.addVideo());
  const tracked = Array.from({ length: 9 }, () => first.addSourceBuffer(MIME));
  first.addSourceBuffer("video/mp4; codecs=é"); // не ASCII
  for (const sourceBuffer of tracked) sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
  const events = eventsOf(journalOf(harness, first));
  assert.equal(events.filter((event) => event.kind === "source_buffer_added").length, 8);
  assert.equal(bytesOf(journalOf(harness, first)).length, 8, "the ninth buffer's bytes are not kept");
  const second = page.attachMediaSource(page.addVideo());
  second.addSourceBuffer(MIME);
  assert.equal(harness.bridge.tracker.recordOf(second)?.key, "ms-2");
  assert.equal(harness.debugLines.length, 1, "one debug line for the whole document");
});

test("the hook never throws into the page even if our observation fails", () => {
  const harness = startHarness();
  const { page, bridge } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  bridge.tracker.bytesAppended = () => {
    throw new Error("observer bug");
  };
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes());
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes());
  assert.equal(sourceBuffer.appended.length, 2, "the browser still got every append");
  assert.deepEqual(harness.debugLines, ["Fastiplayer Bridge: observation failed: appendBuffer"]);
});

test("a blocker that patched the prototype BEFORE us: both wrappers run, bytes are recorded once", () => {
  const page = createFakePage();
  const prototype = page.FakeSourceBuffer.prototype;
  const blockerOriginal = prototype.appendBuffer;
  let blockerCalls = 0;
  prototype.appendBuffer = function (this: unknown, source: ArrayBuffer | ArrayBufferView) {
    blockerCalls += 1;
    return Reflect.apply(blockerOriginal, this, [source]);
  };
  const harness = startHarness({ page });
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes());
  assert.equal(blockerCalls, 1);
  assert.equal(sourceBuffer.appended.length, 1);
  assert.equal(bytesOf(journalOf(harness, mediaSource)).length, 1);
});

test("a blocker that patches the prototype AFTER us: its wrapper calls ours, bytes are recorded once", () => {
  const harness = startHarness();
  const { page } = harness;
  const prototype = page.FakeSourceBuffer.prototype;
  const ourWrapper = prototype.appendBuffer;
  let blockerCalls = 0;
  prototype.appendBuffer = function (this: unknown, source: ArrayBuffer | ArrayBufferView) {
    blockerCalls += 1;
    return Reflect.apply(ourWrapper, this, [source]);
  };
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes());
  assert.equal(blockerCalls, 1);
  assert.equal(sourceBuffer.appended.length, 1);
  assert.equal(bytesOf(journalOf(harness, mediaSource)).length, 1);
});

test("wrappers look like the originals: name, length, property flags, not constructible", () => {
  const page = createFakePage();
  const prototype = page.FakeSourceBuffer.prototype;
  const before = Object.getOwnPropertyDescriptor(prototype, "appendBuffer");
  const offsetBefore = Object.getOwnPropertyDescriptor(prototype, "timestampOffset");
  startHarness({ page });
  const after = Object.getOwnPropertyDescriptor(prototype, "appendBuffer");
  const offsetAfter = Object.getOwnPropertyDescriptor(prototype, "timestampOffset");
  assert.notEqual(after?.value, before?.value, "hook installed");
  assert.equal((after?.value as Function).name, "appendBuffer");
  assert.equal((after?.value as Function).length, (before?.value as Function).length);
  assert.deepEqual([after?.writable, after?.enumerable, after?.configurable], [before?.writable, before?.enumerable, before?.configurable]);
  assert.equal(offsetAfter?.set?.name, "set timestampOffset");
  assert.equal(offsetAfter?.get, offsetBefore?.get, "getter untouched");
  assert.throws(() => Reflect.construct(after?.value as Function, []), TypeError);
});

test("no MediaSource in the window: nothing is hooked, the handshake is still consumed", async () => {
  const page = createFakePage();
  const urlBefore = Object.getOwnPropertyDescriptor(page.FakeURL, "createObjectURL")?.value;
  Reflect.deleteProperty(page.win, "MediaSource");
  const harness = startHarness({ page });
  assert.equal(harness.bridge.hooked, false);
  assert.equal(Object.getOwnPropertyDescriptor(page.FakeURL, "createObjectURL")?.value, urlBefore);
  let pageSawHandshake = false;
  page.win.addEventListener("message", () => {
    pageSawHandshake = true;
  });
  const isolated = handshake(page);
  try {
    isolated.send({ kind: "describe_candidates", request_id: 3 });
    await isolated.waitFor(1);
    assert.equal(pageSawHandshake, false);
    assert.deepEqual(isolated.received[0], { kind: "candidates", request_id: 3, candidates: [] });
  } finally {
    isolated.close();
  }
});

test("a removal that collapses to an empty microsecond range is not reported (the protocol rejects empty ranges)", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(MIME);
  sourceBuffer.remove(1.0000001, 1.0000004); // браузер принял (end > start), но в мкс это 1000000..1000000
  sourceBuffer.remove(1, 1.000001);
  assert.deepEqual(eventsOf(journalOf(harness, mediaSource)).slice(1), [
    { kind: "source_buffer_removed", media_source: "ms-1", source_buffer: 0, start_us: 1_000_000, end: { kind: "at", time_us: 1_000_001 } },
  ]);
});

test("arguments are coerced like WebIDL after a successful call: strings to numbers, String objects to strings", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  const sourceBuffer = mediaSource.addSourceBuffer(new String(MIME) as unknown as string);
  sourceBuffer.remove("5" as unknown as number, "10" as unknown as number);
  sourceBuffer.changeType(new String('video/mp4; codecs="avc1.640028"') as unknown as string);
  sourceBuffer.timestampOffset = "3" as unknown as number;
  assert.deepEqual(eventsOf(journalOf(harness, mediaSource)), [
    { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MIME },
    { kind: "source_buffer_removed", media_source: "ms-1", source_buffer: 0, start_us: 5_000_000, end: { kind: "at", time_us: 10_000_000 } },
    { kind: "source_buffer_type_changed", media_source: "ms-1", source_buffer: 0, mime_type: 'video/mp4; codecs="avc1.640028"' },
    { kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: 3_000_000 },
  ]);
});

test("endOfStream() that changes the duration reports the new duration before the end of stream", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  mediaSource.addSourceBuffer(MIME);
  mediaSource.duration = 10;
  mediaSource.testDurationOnEnd = 9.5; // браузер подрезал длительность до конца буфера
  mediaSource.endOfStream();
  assert.deepEqual(eventsOf(journalOf(harness, mediaSource)).slice(2), [
    { kind: "duration_changed", media_source: "ms-1", duration: { kind: "finite", duration_us: 9_500_000 } },
    { kind: "end_of_stream", media_source: "ms-1" },
  ]);
});

test("an endless finite duration (Twitch in Firefox) reaches the player as a live stream (16d1, decision 319)", () => {
  const harness = startHarness();
  const { page } = harness;
  const mediaSource = page.attachMediaSource(page.addVideo());
  mediaSource.addSourceBuffer(MIME);
  // Twitch в Firefox ставит не `Infinity`, а «максимум»; элемент показывает 9 223 372 036 854,775 с.
  mediaSource.duration = 9_223_372_036_854.775;
  // Эфир кончился записью: конечная длительность в том же MediaSource.
  mediaSource.duration = 3_600;

  assert.deepEqual(eventsOf(journalOf(harness, mediaSource)), [
    { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MIME },
    { kind: "duration_changed", media_source: "ms-1", duration: { kind: "live" } },
    { kind: "duration_changed", media_source: "ms-1", duration: { kind: "finite", duration_us: 3_600_000_000 } },
  ]);
});
