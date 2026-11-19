// Ответ страницы о данных под секундой старта задержанного видео (16b3b, решение 378) для каждой формы источника.
// Ответ журнала MSE проверяет journal-start-coverage.test.ts; здесь — перевод на язык договора, источники без
// журнала и сбой чтения страницы (донесение идёт внутри `play()` страницы и бросать не должно).
import assert from "node:assert/strict";
import { test } from "node:test";

import { heldVideoStartDataOf, type HeldVideoStartDataReaders } from "../../src/page/held-video-start-data.ts";
import type { ElementSource } from "../../src/page/element-source.ts";
import { MediaJournal } from "../../src/page/journal/media-journal.ts";
import { RetainedMemory } from "../../src/page/journal/retained-memory.ts";
import type { MediaSourceRecord } from "../../src/page/mse-tracker.ts";
import { VIDEO_64_6S_MP4 } from "./support/media-samples.ts";

const ELEMENT = {} as Element;

function readersFor(source: ElementSource | (() => never), positionSeconds = 3): HeldVideoStartDataReaders & { readonly oddities: string[] } {
  const oddities: string[] = [];
  return {
    oddities,
    sourceOf: () => (typeof source === "function" ? source() : source),
    currentTimeSeconds: () => positionSeconds,
    reportOddity: (line) => void oddities.push(line),
  };
}

/** Журнал ролика, скачанного одним куском; `evicted` — потолок памяти выкинул этот кусок. */
function mediaSourceOf(options: { readonly evicted: boolean }): ElementSource {
  const journal = new MediaJournal("ms-1", { memory: new RetainedMemory() });
  journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: 'video/mp4; codecs="avc1.42E01E"' });
  journal.recordBytes(0, VIDEO_64_6S_MP4.bytes());
  if (options.evicted) {
    journal.evictOldest();
    journal.evictOldest();
  }
  const record = { key: "ms-1", journal, objectUrl: "blob:reel", mediaSource: new WeakRef({}) } as unknown as MediaSourceRecord;
  return { kind: "media_source", record };
}

test("MediaSource: the journal's answer in the words of the contract", () => {
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor(mediaSourceOf({ evicted: false }))), "available");
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor(mediaSourceOf({ evicted: true }))), "lost");
});

test("MediaSource with an unreadable position (no metadata): unknown, not lost", () => {
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor(mediaSourceOf({ evicted: true }), Number.NaN)), "unknown");
});

test("a file and an HLS playlist are fetched by the background from any second: available", () => {
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor({ kind: "file", url: "https://cdn.example/a.mp4" })), "available");
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor({ kind: "hls", url: "https://cdn.example/a.m3u8" })), "available");
});

test("a worker MediaSource and an element without a source: unknown — the move behaves as before 378", () => {
  const worker = { kind: "worker_media_source", mediaSource: {} } as unknown as ElementSource;
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor(worker)), "unknown");
  assert.equal(heldVideoStartDataOf(ELEMENT, readersFor({ kind: "none" })), "unknown");
});

test("a throwing page read is an honest unknown with a trace, never an exception", () => {
  const readers = readersFor(() => {
    throw new Error("hostile getter");
  });
  assert.equal(heldVideoStartDataOf(ELEMENT, readers), "unknown");
  assert.deepEqual(readers.oddities, ["could not tell whether a held video still has its data"]);
});
