// «Память без копий» (решение 176): журнал MediaSource, потолок памяти, снимок, повтор.
//
// Главное, что закреплено: повтор после выкидывания — это init + поток с границы сегмента, байт в
// байт равный исходному файлу; до передачи журнал держит ссылки (тот же буфер), а не копии.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { LocalMseEvent } from "../../src/shared/handoff-contract.ts";
import { DocumentMemoryBudget } from "../../src/page/journal/memory-budget.ts";
import type { MediaJournal, ReplayItem } from "../../src/page/journal/media-journal.ts";
import { createSegmentScanner } from "../../src/page/segment-scanner/scanner.ts";
import { AUDIO_MP4, VIDEO_64_6S_MP4, VIDEO_MP4, concat, liveWebm, randomChunks, splitAt } from "./support/media-samples.ts";

const KEY = "ms-1";
const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';

function added(sourceBuffer: number, mimeType: string): LocalMseEvent {
  return { kind: "source_buffer_added", media_source: KEY, source_buffer: sourceBuffer, mime_type: mimeType };
}

/** Каждый кусок — в своём буфере (как отдельные загрузки страницы). */
function ownBuffers(chunks: readonly Uint8Array[]): Uint8Array[] {
  return chunks.map((chunk) => chunk.slice());
}

/** Страница передала свой буфер (например, в воркер): у нас он отсоединён, вид пустеет. */
function detachPageBuffer(view: Uint8Array | undefined): void {
  assert.ok(view !== undefined);
  structuredClone(view.buffer, { transfer: [view.buffer as ArrayBuffer] });
  assert.equal(view.byteLength, 0, "detached");
}

function newJournal(capBytes: number, isGone: () => boolean = () => false): { budget: DocumentMemoryBudget; journal: MediaJournal } {
  const budget = new DocumentMemoryBudget(capBytes);
  return { budget, journal: budget.createJournal(KEY, { isGone }) };
}

function appendAll(budget: DocumentMemoryBudget, journal: MediaJournal, sourceBuffer: number, chunks: readonly Uint8Array[]): void {
  for (const chunk of chunks) {
    journal.recordBytes(sourceBuffer, chunk);
    budget.enforce();
  }
}

/** Склейка байтов повтора одного SourceBuffer. */
function replayedBytes(items: readonly ReplayItem[], sourceBuffer: number): Uint8Array {
  return concat(items.filter((item) => item.kind === "bytes" && item.sourceBuffer === sourceBuffer).map((item) => (item.kind === "bytes" ? item.view : new Uint8Array())));
}

function replayedEvents(items: readonly ReplayItem[]): LocalMseEvent[] {
  return items.flatMap((item) => (item.kind === "mse" ? [item.event] : []));
}

test("under the cap: replay is every event and the SAME buffers (references, not copies)", () => {
  const { budget, journal } = newJournal(64 * 1024 * 1024);
  const chunks = ownBuffers(splitAt(VIDEO_MP4.bytes(), [500, 733, 3000, 5714]));
  journal.recordEvent(added(0, VIDEO_MIME));
  journal.recordEvent({ kind: "duration_changed", media_source: KEY, duration: { kind: "finite", duration_us: 400_000 } });
  appendAll(budget, journal, 0, chunks);

  const items = journal.replay();
  assert.deepEqual(replayedEvents(items), [
    added(0, VIDEO_MIME),
    { kind: "duration_changed", media_source: KEY, duration: { kind: "finite", duration_us: 400_000 } },
  ]);
  const byteItems = items.filter((item) => item.kind === "bytes");
  assert.equal(byteItems.length, chunks.length);
  byteItems.forEach((item, index) => {
    assert.equal(item.kind === "bytes" && item.view, chunks[index], `chunk ${index} is the page's own view`);
  });
  assert.equal(budget.memory.totalBytes, VIDEO_MP4.bytes().length);
});

test("over the cap: oldest goes first, cut only at a segment boundary, replay = init + stream from that boundary", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  // Потолок меньше файла: часть начала обязана уйти.
  const capBytes = 12_000;
  for (let seed = 1; seed <= 25; seed += 1) {
    const { budget, journal } = newJournal(capBytes);
    journal.recordEvent(added(0, VIDEO_MIME));
    appendAll(budget, journal, 0, ownBuffers(randomChunks(file, 1500, seed)));
    assert.ok(budget.memory.totalBytes <= capBytes, `seed ${seed}: memory ${budget.memory.totalBytes} under the cap`);

    const items = journal.replay();
    const bytes = replayedBytes(items, 0);
    const init = file.subarray(0, VIDEO_64_6S_MP4.initEnd);
    assert.deepEqual(bytes.subarray(0, init.length), init, `seed ${seed}: replay starts with the init segment`);
    const tailLength = bytes.length - init.length;
    const tailStart = file.length - tailLength;
    assert.ok((VIDEO_64_6S_MP4.mediaStarts as readonly number[]).includes(tailStart), `seed ${seed}: tail starts at a segment boundary (${tailStart})`);
    assert.ok(tailStart > VIDEO_64_6S_MP4.initEnd, `seed ${seed}: something was evicted`);
    assert.deepEqual(bytes.subarray(init.length), file.subarray(tailStart), `seed ${seed}: tail is byte-exact`);
    assert.deepEqual(replayedEvents(items), [added(0, VIDEO_MIME)], "source_buffer_added comes from the snapshot");
  }
});

test("replayed bytes are a valid stream: the scanner reads them as init + whole segments", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const { budget, journal } = newJournal(15_000);
  journal.recordEvent(added(0, VIDEO_MIME));
  appendAll(budget, journal, 0, ownBuffers(randomChunks(file, 900, 42)));
  const scanner = createSegmentScanner(VIDEO_MIME, 0);
  assert.ok(scanner !== null);
  const outcome = scanner.scan(replayedBytes(journal.replay(), 0));
  assert.equal(outcome.kind, "scanned");
  if (outcome.kind !== "scanned") return;
  assert.deepEqual(outcome.markers[0], { kind: "init_start", offset: 0 });
  assert.deepEqual(outcome.markers[1], { kind: "init_end", offset: VIDEO_64_6S_MP4.initEnd });
  assert.ok(outcome.markers.slice(2).every((marker) => marker.kind === "media_start"));
});

test("snapshot keeps MIME after changeType, timestamp offset, duration and an init split over several chunks (views, no copies)", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const { budget, journal } = newJournal(10_000);
  const changedMime = 'video/mp4; codecs="avc1.640028"';
  journal.recordEvent(added(0, VIDEO_MIME));
  journal.recordEvent({ kind: "duration_changed", media_source: KEY, duration: { kind: "finite", duration_us: 6_000_000 } });
  // Init — тремя кусками, с разрезами посреди moov.
  const initChunks = ownBuffers(splitAt(file.subarray(0, VIDEO_64_6S_MP4.initEnd), [10, 300]));
  appendAll(budget, journal, 0, initChunks);
  journal.recordEvent({ kind: "timestamp_offset_changed", media_source: KEY, source_buffer: 0, offset_us: -1_500_000 });
  journal.recordEvent({ kind: "source_buffer_type_changed", media_source: KEY, source_buffer: 0, mime_type: changedMime });
  appendAll(budget, journal, 0, ownBuffers(randomChunks(file.subarray(VIDEO_64_6S_MP4.initEnd), 2000, 9)));

  const items = journal.replay();
  const events = replayedEvents(items);
  assert.deepEqual(events.slice(0, 3), [
    added(0, changedMime),
    { kind: "duration_changed", media_source: KEY, duration: { kind: "finite", duration_us: 6_000_000 } },
    { kind: "timestamp_offset_changed", media_source: KEY, source_buffer: 0, offset_us: -1_500_000 },
  ]);
  // Первые три байтовых записи повтора — виды на те же три куска init (без копий).
  const firstBytes = items.filter((item) => item.kind === "bytes").slice(0, 3);
  firstBytes.forEach((item, index) => {
    assert.ok(item.kind === "bytes");
    assert.equal(item.view.buffer, initChunks[index]?.buffer, `init piece ${index} references the page buffer`);
  });
  assert.deepEqual(replayedBytes(items, 0).subarray(0, VIDEO_64_6S_MP4.initEnd), file.subarray(0, VIDEO_64_6S_MP4.initEnd));
});

test("two interleaved source buffers: each one replays as its init + its stream from a boundary", () => {
  const video = VIDEO_64_6S_MP4.bytes();
  const audio = AUDIO_MP4.bytes();
  const { budget, journal } = newJournal(14_000);
  journal.recordEvent(added(0, VIDEO_MIME));
  journal.recordEvent(added(1, AUDIO_MIME));
  const videoChunks = ownBuffers(randomChunks(video, 1200, 5));
  const audioChunks = ownBuffers(randomChunks(audio, 300, 6));
  for (let index = 0; index < Math.max(videoChunks.length, audioChunks.length); index += 1) {
    const videoChunk = videoChunks[index];
    const audioChunk = audioChunks[index];
    if (videoChunk !== undefined) appendAll(budget, journal, 0, [videoChunk]);
    if (audioChunk !== undefined) appendAll(budget, journal, 1, [audioChunk]);
  }
  const items = journal.replay();
  for (const [sourceBuffer, file, sample] of [
    [0, video, VIDEO_64_6S_MP4],
    [1, audio, AUDIO_MP4],
  ] as const) {
    const bytes = replayedBytes(items, sourceBuffer);
    const init = file.subarray(0, sample.initEnd);
    const tailStart = file.length - (bytes.length - init.length);
    if (tailStart === 0) {
      assert.deepEqual(bytes, file, `source buffer ${sourceBuffer} fully kept`);
      continue;
    }
    assert.deepEqual(bytes.subarray(0, init.length), init, `source buffer ${sourceBuffer} init`);
    // Хвост — с границы сегмента или пуст (всё медиа этого SourceBuffer старше потолка; init остаётся).
    const validStarts = [...sample.mediaStarts, file.length];
    assert.ok(validStarts.includes(tailStart), `source buffer ${sourceBuffer} cut at ${tailStart}`);
    assert.deepEqual(bytes.subarray(init.length), file.subarray(tailStart));
  }
});

test("a format the scanner does not understand makes the journal incomplete: snapshot only, nothing retained", () => {
  const { budget, journal } = newJournal(64 * 1024 * 1024);
  journal.recordEvent(added(0, "audio/mpeg"));
  journal.recordEvent({ kind: "duration_changed", media_source: KEY, duration: { kind: "live" } });
  appendAll(budget, journal, 0, [new Uint8Array([0xff, 0xfb, 0x90, 0x64, 1, 2, 3])]);
  assert.equal(journal.completeness, "incomplete");
  assert.equal(budget.memory.totalBytes, 0, "no references kept");
  assert.deepEqual(journal.replay(), [
    { kind: "mse", event: added(0, "audio/mpeg") },
    { kind: "mse", event: { kind: "duration_changed", media_source: KEY, duration: { kind: "live" } } },
  ]);
});

test("a parse failure mid-stream: journal incomplete, bytes released, replay = snapshot with the known init", () => {
  const file = VIDEO_MP4.bytes();
  const { budget, journal } = newJournal(64 * 1024 * 1024);
  journal.recordEvent(added(0, VIDEO_MIME));
  appendAll(budget, journal, 0, ownBuffers([file.subarray(0, 733), file.subarray(733, 5714)]));
  appendAll(budget, journal, 0, [new Uint8Array([0, 0, 0, 3, 0x6d, 0x6f, 0x6f, 0x66])]); // мусорный бокс
  assert.equal(journal.completeness, "incomplete");
  assert.equal(budget.memory.totalBytes, 733, "only the init chunk stays referenced");
  const items = journal.replay();
  assert.deepEqual(replayedEvents(items), [added(0, VIDEO_MIME)]);
  assert.deepEqual(replayedBytes(items, 0), file.subarray(0, 733));
});

test("evicted chunk ended mid-segment: later chunks without a boundary are dropped, the next boundary resumes", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const { budget, journal } = newJournal(9_000);
  journal.recordEvent(added(0, VIDEO_MIME));
  // init (734) + первая половина сегмента 1: потолок 9000 ещё не превышен.
  appendAll(budget, journal, 0, ownBuffers([file.subarray(0, 734), file.subarray(734, 4000)]));
  // Вторая половина сегмента 1 (6000+ байт): превышение → выкинут кусок 734..4000 посреди сегмента,
  // и сама вторая половина без границы становится бесполезной.
  appendAll(budget, journal, 0, ownBuffers([file.subarray(4000, 7258)]));
  // Следующий сегмент — с границы.
  appendAll(budget, journal, 0, ownBuffers([file.subarray(7258, 13398)]));
  const bytes = replayedBytes(journal.replay(), 0);
  assert.deepEqual(bytes, concat([file.subarray(0, 734), file.subarray(7258, 13398)]));
  assert.ok(budget.memory.totalBytes <= 9_000);
});

test("cap is shared by the document: the globally oldest entry goes first", () => {
  const budget = new DocumentMemoryBudget(7_000);
  const older = budget.createJournal("ms-1", { isGone: () => false });
  const newer = budget.createJournal("ms-2", { isGone: () => false });
  const file = VIDEO_MP4.bytes();
  older.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: VIDEO_MIME });
  newer.recordEvent({ kind: "source_buffer_added", media_source: "ms-2", source_buffer: 0, mime_type: VIDEO_MIME });
  appendAll(budget, older, 0, ownBuffers([file.subarray(0, 733), file.subarray(733, 5714)]));
  appendAll(budget, newer, 0, ownBuffers([file.subarray(0, 733), file.subarray(733, 5714)]));
  // Старший журнал отдал свой медиасегмент, младший — цел.
  assert.deepEqual(replayedBytes(older.replay(), 0), file.subarray(0, 733));
  assert.deepEqual(replayedBytes(newer.replay(), 0), file.subarray(0, 5714));
});

test("journals of gone media sources are discarded when a new one starts", () => {
  let gone = false;
  const budget = new DocumentMemoryBudget(64 * 1024 * 1024);
  const first = budget.createJournal("ms-1", { isGone: () => gone });
  first.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: VIDEO_MIME });
  appendAll(budget, first, 0, ownBuffers([VIDEO_MP4.bytes()]));
  assert.equal(budget.memory.totalBytes, VIDEO_MP4.bytes().length);
  gone = true;
  budget.createJournal("ms-2", { isGone: () => false });
  assert.equal(budget.memory.totalBytes, 0, "everything of the gone media source is released");
});

test("one big page buffer cut into views is counted once", () => {
  const file = VIDEO_MP4.bytes().slice();
  const { budget, journal } = newJournal(64 * 1024 * 1024);
  journal.recordEvent(added(0, VIDEO_MIME));
  appendAll(budget, journal, 0, splitAt(file, [733, 5714])); // три вида на один буфер
  assert.equal(budget.memory.totalBytes, file.buffer.byteLength);
});

test("tiny chunks (headers split over many chunks): replay is still init + exact stream from a boundary, fMP4 and live WebM", () => {
  const webm = liveWebm(40);
  const cases = [
    { mime: VIDEO_MIME, file: VIDEO_64_6S_MP4.bytes(), initEnd: VIDEO_64_6S_MP4.initEnd, starts: VIDEO_64_6S_MP4.mediaStarts as readonly number[], cap: 12_000 },
    { mime: "video/webm", file: webm.bytes, initEnd: webm.initEnd, starts: webm.mediaStarts, cap: 6_000 },
  ];
  for (const { mime, file, initEnd, starts, cap } of cases) {
    for (let seed = 1; seed <= 10; seed += 1) {
      const { budget, journal } = newJournal(cap);
      journal.recordEvent(added(0, mime));
      appendAll(budget, journal, 0, ownBuffers(randomChunks(file, 12, seed)));
      assert.ok(budget.memory.totalBytes <= cap, `${mime} seed ${seed}: under the cap`);
      const bytes = replayedBytes(journal.replay(), 0);
      assert.deepEqual(bytes.subarray(0, initEnd), file.subarray(0, initEnd), `${mime} seed ${seed}: init is exact`);
      const tailStart = file.length - (bytes.length - initEnd);
      assert.ok([...starts, file.length].includes(tailStart), `${mime} seed ${seed}: tail starts at a boundary (${tailStart})`);
      assert.ok(tailStart > initEnd, `${mime} seed ${seed}: something was evicted`);
      assert.deepEqual(bytes.subarray(initEnd), file.subarray(tailStart), `${mime} seed ${seed}: tail is exact`);
    }
  }
});

test("the page detached a kept buffer: replay makes the journal incomplete (snapshot + init), no silent hole", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const { budget, journal } = newJournal(64 * 1024 * 1024);
  journal.recordEvent(added(0, VIDEO_MIME));
  const chunks = ownBuffers(splitAt(file, [VIDEO_64_6S_MP4.initEnd, 7258, 13398]));
  appendAll(budget, journal, 0, chunks);
  // Страница передала буфер второго сегмента в воркер: наш вид опустел.
  detachPageBuffer(chunks[2]);
  const items = journal.replay();
  assert.equal(journal.completeness, "incomplete");
  assert.deepEqual(replayedEvents(items), [added(0, VIDEO_MIME)]);
  assert.deepEqual(replayedBytes(items, 0), file.subarray(0, VIDEO_64_6S_MP4.initEnd), "only the intact init");
});

test("the page detached the init buffer: replay is the snapshot without init bytes", () => {
  const file = VIDEO_MP4.bytes();
  const { budget, journal } = newJournal(64 * 1024 * 1024);
  journal.recordEvent(added(0, VIDEO_MIME));
  const chunks = ownBuffers(splitAt(file, [733]));
  appendAll(budget, journal, 0, chunks);
  detachPageBuffer(chunks[0]);
  const items = journal.replay();
  assert.equal(journal.completeness, "incomplete");
  assert.deepEqual(replayedBytes(items, 0).length, 0);
});

test("evicting a chunk whose buffer the page detached: replay stays complete and exact (the detached chunk is gone anyway)", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const { budget, journal } = newJournal(12_000);
  journal.recordEvent(added(0, VIDEO_MIME));
  // Сегмент 1 двумя кусками; первый кусок страница сразу отсоединяет (уйдёт при выкидывании).
  const head = ownBuffers([file.subarray(0, 734), file.subarray(734, 3000)]);
  appendAll(budget, journal, 0, head);
  detachPageBuffer(head[1]);
  appendAll(budget, journal, 0, ownBuffers(splitAt(file.subarray(3000), [7258 - 3000, 13398 - 3000, 18437 - 3000, 24583 - 3000])));
  const bytes = replayedBytes(journal.replay(), 0);
  assert.equal(journal.completeness, "complete");
  const tailStart = file.length - (bytes.length - 734);
  assert.ok((VIDEO_64_6S_MP4.mediaStarts as readonly number[]).includes(tailStart), `tail starts at a boundary (${tailStart})`);
  assert.deepEqual(bytes.subarray(734), file.subarray(tailStart));
});
