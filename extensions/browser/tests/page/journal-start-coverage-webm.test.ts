// Есть ли в повторе журнала данные под секундой старта для WebM-ролика (16b3c, решение 379): раньше при
// потерянном начале журнал отвечал `unknown`, и возврат к такому ролику мог ждать без предела (дыра решения
// 378). Теперь время кластера читается из его `Timestamp`, и ответ `lost`/`covered` — настоящий. Проверяется
// на цепочке «журнал → ответ сторожу» с настоящими байтами EBML, как для fMP4 в journal-start-coverage.test.ts.
import assert from "node:assert/strict";
import { test } from "node:test";

import type { ElementSource } from "../../src/page/element-source.ts";
import { heldVideoStartDataOf } from "../../src/page/held-video-start-data.ts";
import { MediaJournal } from "../../src/page/journal/media-journal.ts";
import { RetainedMemory } from "../../src/page/journal/retained-memory.ts";
import type { MediaSourceRecord } from "../../src/page/mse-tracker.ts";
import { concat, EBML_IDS, ebmlElement, filler } from "./support/media-samples.ts";

const WEBM_MIME = 'video/webm; codecs="vp9, opus"';
const TIMECODE_SCALE_ID = 0x2ad7b1;
/** Кластеры ролика: 0; 1,2; 2,4; 3,6; 4,8 с (шкала 1 мс, Timestamp в миллисекундах). */
const CLUSTER_SECONDS = [0, 1.2, 2.4, 3.6, 4.8] as const;
const ELEMENT = {} as Element;

/** Число в 2 байта, старший первым (хватает для миллисекунд теста). */
function twoBytes(value: number): Uint8Array {
  return new Uint8Array([Math.floor(value / 256), value % 256]);
}

function webmInit(infoChildren: readonly Uint8Array[]): Uint8Array {
  const segmentBody = concat([ebmlElement(EBML_IDS.info, concat(infoChildren)), ebmlElement(EBML_IDS.tracks, filler(30))]);
  return concat([ebmlElement(EBML_IDS.header, filler(19)), ebmlElement(EBML_IDS.segment, segmentBody)]);
}

/** Кластер с заданным Timestamp (в тиках) и блоком данных. */
function webmCluster(timestampTicks: number): Uint8Array {
  return ebmlElement(EBML_IDS.cluster, concat([ebmlElement(EBML_IDS.timestamp, twoBytes(timestampTicks)), ebmlElement(EBML_IDS.simpleBlock, filler(200, timestampTicks))]));
}

const MILLISECOND_SCALE = ebmlElement(TIMECODE_SCALE_ID, new Uint8Array([0x0f, 0x42, 0x40]));

/** Страница положила init и все кластеры, каждый своей загрузкой. */
function journalOfWebmReel(options: { readonly init?: Uint8Array; readonly offsetUs?: number } = {}): MediaJournal {
  const journal = new MediaJournal("ms-1", { memory: new RetainedMemory() });
  journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: WEBM_MIME });
  if (options.offsetUs !== undefined) journal.recordEvent({ kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: options.offsetUs });
  journal.recordBytes(0, (options.init ?? webmInit([MILLISECOND_SCALE])).slice());
  for (const seconds of CLUSTER_SECONDS) journal.recordBytes(0, webmCluster(Math.round(seconds * 1000)));
  return journal;
}

function evictOldest(journal: MediaJournal, count: number): void {
  for (let evicted = 0; evicted < count; evicted += 1) journal.evictOldest();
}

test("WebM, the head was evicted: covered at or after the first kept cluster, lost before it", () => {
  const journal = journalOfWebmReel();
  // Выкинуты событие, init и два первых кластера (0 и 1,2 с): повтор начнётся с кластера 2,4 с.
  evictOldest(journal, 4);
  assert.equal(journal.startCoverageAt(3), "covered", "a cluster at 2.4 s lies under 3 s");
  assert.equal(journal.startCoverageAt(2.4), "covered", "a boundary exactly at the position counts");
  assert.equal(journal.startCoverageAt(2), "lost", "the data under 2 s is gone");
});

test("WebM, nothing evicted or only the init piece evicted: covered", () => {
  const journal = journalOfWebmReel();
  assert.equal(journal.startCoverageAt(0), "covered");
  evictOldest(journal, 2);
  assert.equal(journal.startCoverageAt(0), "covered", "evicting the init piece alone loses no media");
});

test("WebM, timestampOffset shifts cluster times onto the page timeline", () => {
  const journal = journalOfWebmReel({ offsetUs: 6_000_000 });
  // Выкинуты события добавления и сдвига, init и два кластера: первый оставшийся — 2,4 + 6 = 8,4 с.
  evictOldest(journal, 5);
  assert.equal(journal.startCoverageAt(8.5), "covered");
  assert.equal(journal.startCoverageAt(8.3), "lost");
  assert.equal(journal.startCoverageAt(3), "lost", "without the offset 3 s would wrongly look covered by 2.4 s");
});

test("WebM, TimecodeScale other than 1 ms is honoured", () => {
  // Шкала 100 мкс (100 000 нс на тик): Timestamp 24 000 тиков = 2,4 с. Если бы журнал считал тик
  // миллисекундой, 24 000 тиков вышли бы 24 с и ответ ниже был бы другим.
  const fineScale = ebmlElement(TIMECODE_SCALE_ID, new Uint8Array([0x01, 0x86, 0xa0]));
  const journal = new MediaJournal("ms-1", { memory: new RetainedMemory() });
  journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: WEBM_MIME });
  journal.recordBytes(0, webmInit([fineScale]));
  for (const ticks of [0, 12_000, 24_000, 36_000]) {
    journal.recordBytes(0, ebmlElement(EBML_IDS.cluster, concat([ebmlElement(EBML_IDS.timestamp, twoBytes(ticks)), ebmlElement(EBML_IDS.simpleBlock, filler(50))])));
  }
  // Выкинуты событие, init и два первых кластера (0 и 1,2 с): первый оставшийся — 24 000 тиков = 2,4 с.
  evictOldest(journal, 4);
  assert.equal(journal.startCoverageAt(2.5), "covered");
  assert.equal(journal.startCoverageAt(2.3), "lost");
});

test("WebM without TimecodeScale in Info uses the 1 ms default", () => {
  const journal = journalOfWebmReel({ init: webmInit([ebmlElement(0x4d80, filler(5))]) });
  evictOldest(journal, 4);
  assert.equal(journal.startCoverageAt(3), "covered");
  assert.equal(journal.startCoverageAt(2), "lost");
});

test("WebM with an unreadable cluster time stays unknown, never lost", () => {
  const journal = new MediaJournal("ms-1", { memory: new RetainedMemory() });
  journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: WEBM_MIME });
  journal.recordBytes(0, webmInit([MILLISECOND_SCALE]));
  // Кластеры без Timestamp (битый поток): границы сканер видит, времени нет.
  for (let cluster = 0; cluster < 3; cluster += 1) {
    journal.recordBytes(0, ebmlElement(EBML_IDS.cluster, ebmlElement(EBML_IDS.simpleBlock, filler(60))));
  }
  evictOldest(journal, 3);
  assert.equal(journal.startCoverageAt(3), "unknown");
});

/** Ответ, который сторож донесёт фону о ролике (`start_data`), для журнала WebM-ролика. */
function heldStartDataOf(journal: MediaJournal, positionSeconds: number): string {
  const record = { key: "ms-1", journal, objectUrl: "blob:reel", mediaSource: new WeakRef({}) } as unknown as MediaSourceRecord;
  const source: ElementSource = { kind: "media_source", record };
  return heldVideoStartDataOf(ELEMENT, { sourceOf: () => source, currentTimeSeconds: () => positionSeconds, reportOddity: () => undefined });
}

test("the guard's report for a WebM reel with a lost start carries lost, so the background releases it to the browser", () => {
  const journal = journalOfWebmReel();
  assert.equal(heldStartDataOf(journal, 3), "available", "nothing evicted: the data is there");
  // Память отдала начало ролика: кластеры 0, 1,2 с. Игралось до 2-й секунды — данных под ней у моста нет.
  evictOldest(journal, 4);
  assert.equal(heldStartDataOf(journal, 2), "lost");
  assert.equal(heldStartDataOf(journal, 3), "available", "a position under a kept cluster still has its data");
});
