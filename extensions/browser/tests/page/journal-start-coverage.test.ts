// Есть ли в повторе журнала данные под секундой, с которой видео пойдёт в окне (16b3b, решение 378).
//
// Вернувшийся ролик окно берёт, только если журнал не потерял данные под его позицией: страница то, что уже
// положила в свой SourceBuffer, второй раз не пришлёт. Проверяется ответ журнала на настоящих байтах fMP4 и
// WebM: init без медиасегментов, выкинутая голова, позиция за последним сегментом, `timestampOffset`.
import assert from "node:assert/strict";
import { test } from "node:test";

import { MediaJournal } from "../../src/page/journal/media-journal.ts";
import { RetainedMemory } from "../../src/page/journal/retained-memory.ts";
import { MUXED_WEBM, VIDEO_64_6S_MP4 } from "./support/media-samples.ts";

const MP4_MIME = 'video/mp4; codecs="avc1.42E01E"';
const WEBM_MIME = 'video/webm; codecs="vp9, opus"';
const FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
/** Границы медиасегментов фикстуры и конец файла; сегменты начинаются на 0; 1,2; 2,4; 3,6; 4,8 с. */
const SEGMENT_EDGES: readonly number[] = [...VIDEO_64_6S_MP4.mediaStarts, FILE.length];
const SEGMENT_COUNT = SEGMENT_EDGES.length - 1;

function openJournal(mimeType: string): MediaJournal {
  const journal = new MediaJournal("ms-1", { memory: new RetainedMemory() });
  journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: mimeType });
  return journal;
}

/** Страница кладёт init и медиасегменты `from..to` (номера с нуля), каждый своей загрузкой. */
function appendMp4(journal: MediaJournal, segments: { readonly from: number; readonly to: number }): void {
  journal.recordBytes(0, FILE.slice(0, INIT_END));
  for (let segment = segments.from; segment < segments.to; segment += 1) {
    journal.recordBytes(0, FILE.slice(SEGMENT_EDGES[segment] ?? 0, SEGMENT_EDGES[segment + 1] ?? 0));
  }
}

/** Потолок памяти выкинул `count` самых старых записей журнала (событие `source_buffer_added` — первая из них). */
function evictOldest(journal: MediaJournal, count: number): void {
  for (let evicted = 0; evicted < count; evicted += 1) journal.evictOldest();
}

test("init only and nothing evicted: covered — the page has not downloaded more, the data will come live", () => {
  const journal = openJournal(MP4_MIME);
  journal.recordBytes(0, FILE.slice(0, INIT_END));
  assert.equal(journal.startCoverageAt(0), "covered");
  assert.equal(journal.startCoverageAt(3), "covered");
});

for (const sample of [
  { name: "fMP4", mime: MP4_MIME, bytes: () => FILE.slice(0, INIT_END) },
  { name: "WebM", mime: WEBM_MIME, bytes: () => MUXED_WEBM.bytes().slice(0, MUXED_WEBM.initEnd) },
]) {
  test(`${sample.name}: the lone init piece evicted before any media came — covered, nothing was lost`, () => {
    // Дальний ещё не игравший ролик (ступень 5) отдал свой init-кусок раньше, чем страница положила первый
    // медиасегмент: init остаётся в снимке, медиаданных журнал не выкидывал — данные придут живым потоком.
    const journal = openJournal(sample.mime);
    journal.recordBytes(0, sample.bytes());
    evictOldest(journal, 2);
    assert.equal(journal.startCoverageAt(0), "covered");
  });
}

test("init only after the whole video was evicted: lost — the page holds its data and will not send it again", () => {
  const journal = openJournal(MP4_MIME);
  // Страница скачала ролик одним куском, потолок памяти его выкинул: в повторе остался init из снимка.
  journal.recordBytes(0, FILE.slice());
  evictOldest(journal, 2);
  assert.equal(journal.startCoverageAt(3), "lost");
  assert.equal(journal.startCoverageAt(0), "lost");
});

test("evicting the init piece alone loses no media: the replay still covers every second", () => {
  const journal = openJournal(MP4_MIME);
  appendMp4(journal, { from: 0, to: SEGMENT_COUNT });
  // Событие и кусок init: init остаётся в снимке, медиасегменты все на месте.
  evictOldest(journal, 2);
  assert.equal(journal.startCoverageAt(0), "covered");
});

test("the head was evicted: covered at or after the first kept segment, lost before it", () => {
  const journal = openJournal(MP4_MIME);
  appendMp4(journal, { from: 0, to: SEGMENT_COUNT });
  // Выкинуты событие, init и два первых сегмента (0 и 1,2 с): повтор начнётся с сегмента 2,4 с.
  evictOldest(journal, 4);
  assert.equal(journal.startCoverageAt(3), "covered", "a segment at 2.4 s lies under 3 s");
  assert.equal(journal.startCoverageAt(2.4), "covered", "a boundary exactly at the position counts");
  assert.equal(journal.startCoverageAt(2), "lost", "the data under 2 s is gone");
});

test("a position past the last kept segment is covered: the journal drops only its head, so anything the page added later is in it", () => {
  const journal = openJournal(MP4_MIME);
  // Страница скачала только три первых сегмента (до 3,6 с), журнал выкинул первый.
  appendMp4(journal, { from: 0, to: 3 });
  evictOldest(journal, 3);
  // На 5,5 с у страницы данных нет (иначе они лежали бы в журнале после 2,4 с): она их скачает, и они придут.
  assert.equal(journal.startCoverageAt(5.5), "covered");
  assert.equal(journal.startCoverageAt(1), "lost");
});

test("timestampOffset shifts segment times onto the page timeline", () => {
  const journal = openJournal(MP4_MIME);
  // Второй ролик ленты в том же SourceBuffer: страница сдвинула шкалу на 6 с.
  journal.recordEvent({ kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: 6_000_000 });
  appendMp4(journal, { from: 0, to: SEGMENT_COUNT });
  // Выкинуты событие добавления, событие сдвига, init и два сегмента: первый оставшийся — 2,4 + 6 = 8,4 с.
  evictOldest(journal, 5);
  assert.equal(journal.startCoverageAt(8.5), "covered");
  assert.equal(journal.startCoverageAt(8.3), "lost", "8.3 s on the page is before the first kept segment at 8.4 s");
  assert.equal(journal.startCoverageAt(3), "lost", "without the offset 3 s would wrongly look covered by 2.4 s");
});

test("WebM with everything evicted is lost: no segment is left, its time is not needed", () => {
  const webm = MUXED_WEBM.bytes();
  const journal = openJournal(WEBM_MIME);
  journal.recordBytes(0, webm.slice());
  evictOldest(journal, 2);
  assert.equal(journal.startCoverageAt(0), "lost");
});

test("an incomplete journal is unknown: its format was not understood", () => {
  const journal = openJournal(MP4_MIME);
  appendMp4(journal, { from: 0, to: 2 });
  journal.markIncomplete();
  assert.equal(journal.startCoverageAt(0), "unknown");
});
