// «Уже просмотренное» в одном журнале (решение 364): время сегмента fMP4 и выкидывание записей, лежащих
// позади названной секунды страницы.
//
// Главное, что закреплено: после `evictOldestIfWatchedBefore(T)` повтор всё ещё начинается с границы
// сегмента не позже T (передача с текущей секунды страницы найдёт данные), а отказ ничего не меняет.

import assert from "node:assert/strict";
import { test } from "node:test";

import { VIEWING_UNKNOWN } from "../../src/page/journal/journal-viewing.ts";
import { DocumentMemoryBudget } from "../../src/page/journal/memory-budget.ts";
import type { MediaJournal } from "../../src/page/journal/media-journal.ts";
import { readFmp4FragmentDecodeTime, readFmp4TrackTimescales, type StreamByteReader } from "../../src/page/segment-scanner/fmp4-segment-time.ts";
import { AUDIO_MP4, VIDEO_64_6S_MP4, VIDEO_MP4, concat, liveWebm, randomChunks } from "./support/media-samples.ts";

const KEY = "ms-1";
const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';
const FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
const SEGMENT_STARTS: readonly number[] = VIDEO_64_6S_MP4.mediaStarts;
/** Времена начала сегментов фикстуры, с: шкала 12800, фрагменты по 15360 единиц (ffprobe: 1,2 с). */
const SEGMENT_SECONDS = [0, 1.2, 2.4, 3.6, 4.8];
/** Потолок, до которого тесты не доходят: выкидывает только проверяемый метод. */
const ROOMY_CAP = 64 * 1024 * 1024;

/** Читатель байтов целого файла, как их отдал бы журнал. */
function readerOf(bytes: Uint8Array): StreamByteReader {
  return (offset, length) => (offset >= 0 && offset + length <= bytes.length ? bytes.slice(offset, offset + length) : null);
}

function bigEndian(value: number, byteCount: number): number[] {
  const bytes: number[] = [];
  let rest = value;
  for (let index = 0; index < byteCount; index += 1) {
    bytes.unshift(rest % 256);
    rest = Math.floor(rest / 256);
  }
  return bytes;
}

/** Бокс MP4: размер, тип, тело. */
function box(type: string, ...body: readonly (Uint8Array | readonly number[])[]): Uint8Array {
  const payload = concat(body.map((part) => (part instanceof Uint8Array ? part : new Uint8Array(part))));
  return concat([new Uint8Array([...bigEndian(8 + payload.length, 4), ...[...type].map((letter) => letter.charCodeAt(0))]), payload]);
}

/** Фрагмент с 64-битным временем (tfdt версии 1) и боксами сегмента перед ним, как у живых потоков DASH. */
function wideFragment(trackId: number, decodeTime: number): Uint8Array {
  const fullBox = (version: number): number[] => [version, 0, 0, 0];
  return concat([
    box("styp", [...bigEndian(0, 8)]),
    box("emsg", [...bigEndian(0, 40)]),
    box("moof", box("mfhd", fullBox(0), bigEndian(7, 4)), box("traf", box("tfhd", fullBox(0), bigEndian(trackId, 4)), box("tfdt", fullBox(1), bigEndian(decodeTime, 8)))),
    box("mdat", [1, 2, 3, 4]),
  ]);
}

function newJournal(): { budget: DocumentMemoryBudget; journal: MediaJournal } {
  const budget = new DocumentMemoryBudget(ROOMY_CAP);
  const journal = budget.createJournal(KEY, { isGone: () => false, viewing: () => VIEWING_UNKNOWN });
  return { budget, journal };
}

function addSourceBuffer(journal: MediaJournal, sourceBuffer: number, mimeType: string): void {
  journal.recordEvent({ kind: "source_buffer_added", media_source: KEY, source_buffer: sourceBuffer, mime_type: mimeType });
}

/** Каждый кусок — отдельная загрузка страницы (свой буфер). */
function append(journal: MediaJournal, sourceBuffer: number, chunks: readonly Uint8Array[]): void {
  for (const chunk of chunks) journal.recordBytes(sourceBuffer, chunk.slice());
}

/** Файл по сегментам: init и каждый медиасегмент своей загрузкой. */
function segmentsOf(file: Uint8Array, initEnd: number, mediaStarts: readonly number[]): Uint8Array[] {
  const edges = [0, initEnd, ...mediaStarts.slice(1), file.length];
  return edges.slice(0, -1).map((start, index) => file.subarray(start, edges[index + 1]));
}

function replayedBytes(journal: MediaJournal, sourceBuffer: number): Uint8Array {
  return concat(journal.replay().flatMap((item) => (item.kind === "bytes" && item.sourceBuffer === sourceBuffer ? [item.view] : [])));
}

/** Выкидывать просмотренное до секунды `watchedUntil`, пока журнал соглашается; сколько раз согласился. */
function evictWatched(journal: MediaJournal, watchedUntil: number): number {
  let evictions = 0;
  while (journal.evictOldestIfWatchedBefore(watchedUntil)) evictions += 1;
  return evictions;
}

test("fmp4 segment time: decode time of each fragment and the track timescale are read from real files", () => {
  const video = readerOf(FILE);
  assert.deepEqual(readFmp4TrackTimescales(video, 0, INIT_END), new Map([[1, 12_800]]));
  assert.deepEqual(
    SEGMENT_STARTS.map((start) => readFmp4FragmentDecodeTime(video, start)),
    SEGMENT_SECONDS.map((seconds) => ({ trackId: 1, decodeTime: Math.round(seconds * 12_800) })),
  );
  // Граница журнала бывает и на начале init-сегмента: время такого места — время первого фрагмента за ним.
  assert.deepEqual(readFmp4FragmentDecodeTime(video, 0), { trackId: 1, decodeTime: 0 });
  const audio = readerOf(AUDIO_MP4.bytes());
  assert.deepEqual(readFmp4TrackTimescales(audio, 0, AUDIO_MP4.initEnd), new Map([[1, 48_000]]));
  assert.deepEqual(readFmp4FragmentDecodeTime(audio, AUDIO_MP4.mediaStarts[1]), { trackId: 1, decodeTime: 10_240 });
});

test("fmp4 segment time: a 64-bit decode time behind styp and emsg is read; what cannot be read is unknown, not a guess", () => {
  // Эфир, идущий годами: время не влезает в 32 бита.
  const liveDecodeTime = 2 ** 40 + 12_345;
  const fragment = wideFragment(3, liveDecodeTime);
  assert.deepEqual(readFmp4FragmentDecodeTime(readerOf(fragment), 0), { trackId: 3, decodeTime: liveDecodeTime });

  // Заголовок ещё не пришёл целиком (страница режет где угодно).
  assert.equal(readFmp4FragmentDecodeTime(readerOf(fragment.subarray(0, fragment.length - 20)), 0), null);
  // Место, начинающееся с данных, — не начало сегмента.
  assert.equal(readFmp4FragmentDecodeTime(readerOf(box("mdat", [1, 2, 3])), 0), null);
  // Фрагмент без времени декодирования.
  const withoutDecodeTime = box("moof", box("traf", box("tfhd", [0, 0, 0, 0], bigEndian(1, 4))));
  assert.equal(readFmp4FragmentDecodeTime(readerOf(withoutDecodeTime), 0), null);
  // Мусор вместо бокса и init без `moov`.
  assert.equal(readFmp4FragmentDecodeTime(readerOf(new Uint8Array(64)), 0), null);
  assert.equal(readFmp4TrackTimescales(readerOf(FILE), INIT_END, FILE.length), null);
});

test("watched part: entries go only while the replay would still start at or before the named second", () => {
  const { budget, journal } = newJournal();
  addSourceBuffer(journal, 0, VIDEO_MIME);
  append(journal, 0, segmentsOf(FILE, INIT_END, SEGMENT_STARTS));

  // Просмотрено до 3-й секунды: уходят сегменты 0 с и 1,2 с (после них повтор начнётся с 2,4 с ≤ 3).
  evictWatched(journal, 3);
  assert.deepEqual(replayedBytes(journal, 0), concat([FILE.subarray(0, INIT_END), FILE.subarray(SEGMENT_STARTS[2])]));
  assert.equal(budget.memory.totalBytes, FILE.length - ((SEGMENT_STARTS[2] ?? 0) - INIT_END), "the evicted chunks are released");

  // Отказ ничего не меняет: сегмент 2,4 с остаётся, пока следующая граница (3,6 с) позже названной секунды.
  const memoryBefore = budget.memory.totalBytes;
  assert.equal(journal.evictOldestIfWatchedBefore(3.59), false);
  assert.equal(budget.memory.totalBytes, memoryBefore);
  assert.deepEqual(replayedBytes(journal, 0), concat([FILE.subarray(0, INIT_END), FILE.subarray(SEGMENT_STARTS[2])]));
  // Граница ровно на названной секунде — уже можно.
  assert.equal(journal.evictOldestIfWatchedBefore(3.6), true);
  assert.deepEqual(replayedBytes(journal, 0), concat([FILE.subarray(0, INIT_END), FILE.subarray(SEGMENT_STARTS[3])]));
});

test("watched part: the last segment never goes this way — the replay would be left without a start", () => {
  const { journal } = newJournal();
  addSourceBuffer(journal, 0, VIDEO_MIME);
  append(journal, 0, segmentsOf(FILE, INIT_END, SEGMENT_STARTS));

  // Зритель досмотрел до конца и дальше: всё, кроме последнего сегмента, позади.
  evictWatched(journal, 1_000);
  assert.deepEqual(replayedBytes(journal, 0), concat([FILE.subarray(0, INIT_END), FILE.subarray(SEGMENT_STARTS[4])]));
  assert.equal(journal.evictOldestIfWatchedBefore(1_000), false);
});

test("watched part: the page's timestampOffset moves segment times, and a later download is seen once it arrives", () => {
  const { journal } = newJournal();
  addSourceBuffer(journal, 0, VIDEO_MIME);
  const [init, ...media] = segmentsOf(FILE, INIT_END, SEGMENT_STARTS);
  assert.ok(init !== undefined);
  append(journal, 0, [init, ...media]);
  // Просмотрено до 7-й секунды, а дальше 4,8 с в журнале границ нет: последний сегмент держится.
  evictWatched(journal, 7);
  assert.equal(journal.evictOldestIfWatchedBefore(7), false);
  // Страница кладёт те же сегменты со сдвигом 6 с: теперь они играются с 6 до 12 с.
  journal.recordEvent({ kind: "timestamp_offset_changed", media_source: KEY, source_buffer: 0, offset_us: 6_000_000 });
  append(journal, 0, media);

  // Сегмент 4,8 с уходит (следующая граница — 6,0 с), сегмент 6,0 с — нет (следующая — 7,2 с).
  assert.equal(evictWatched(journal, 7), 1);
  assert.deepEqual(replayedBytes(journal, 0), concat([init, ...media]), "the replay starts at the segment of 6.0 s");
  const offsets = journal.replay().flatMap((item) => (item.kind === "mse" && item.event.kind === "timestamp_offset_changed" ? [item.event.offset_us] : []));
  assert.deepEqual(offsets, [6_000_000], "the offset the page set is replayed before those bytes");
});

test("watched part: with video and audio in one journal each source buffer is cut by its own segment times", () => {
  const { journal } = newJournal();
  addSourceBuffer(journal, 0, VIDEO_MIME);
  addSourceBuffer(journal, 1, AUDIO_MIME);
  const video = VIDEO_MP4.bytes();
  const audio = AUDIO_MP4.bytes();
  // Сегменты: видео 0 и 0,2 с; звук 0 и 0,2133 с. Страница кладёт их вперемешку.
  const videoChunks = segmentsOf(video, VIDEO_MP4.initEnd, VIDEO_MP4.mediaStarts);
  const audioChunks = segmentsOf(audio, AUDIO_MP4.initEnd, AUDIO_MP4.mediaStarts);
  for (let index = 0; index < videoChunks.length; index += 1) {
    append(journal, 0, [videoChunks[index] ?? new Uint8Array()]);
    append(journal, 1, [audioChunks[index] ?? new Uint8Array()]);
  }

  // Просмотрено до 0,21 с: у видео следующая граница 0,2 с — позади; у звука 0,2133 с — ещё нет.
  evictWatched(journal, 0.21);
  assert.deepEqual(replayedBytes(journal, 0), concat([video.subarray(0, VIDEO_MP4.initEnd), video.subarray(VIDEO_MP4.mediaStarts[1])]));
  assert.deepEqual(replayedBytes(journal, 1), audio, "the audio segment that still covers the playhead stays");
});

test("watched part: chunks cut anywhere (headers split between chunks) give the same answer", () => {
  const expected = concat([FILE.subarray(0, INIT_END), FILE.subarray(SEGMENT_STARTS[2])]);
  for (let seed = 1; seed <= 20; seed += 1) {
    const { journal } = newJournal();
    addSourceBuffer(journal, 0, VIDEO_MIME);
    append(journal, 0, randomChunks(FILE, 40, seed));
    evictWatched(journal, 3);
    // Кусок с границей 2,4 с начинается раньше неё: повтор берёт его с границы.
    assert.deepEqual(replayedBytes(journal, 0), expected, `seed ${seed}`);
  }
});

test("watched part: WebM cluster times are read (decision 379), so a watched head is given back", () => {
  const webm = liveWebm(6);
  const { journal: webmJournal } = newJournal();
  addSourceBuffer(webmJournal, 0, "video/webm");
  append(webmJournal, 0, segmentsOf(webm.bytes, webm.initEnd, webm.mediaStarts));
  // Кластеры живого WebM стартуют на 0, 1, 2 … мс (шкала 1 мс). Просмотрено 2,5 мс: кластеры 0 и 1 позади
  // (следующие границы 1 и 2 мс), кластер 2 (следующая граница 3 мс) ещё нет.
  assert.ok(evictWatched(webmJournal, 0.0025) > 0, "the watched head is given back");
  assert.deepEqual(replayedBytes(webmJournal, 0), concat([webm.bytes.subarray(0, webm.initEnd), webm.bytes.subarray(webm.mediaStarts[2])]));
});

test("watched part: unknown segment time is never taken for watched (WebM without Timestamp, a detached page buffer)", () => {
  const webm = liveWebm(6);
  // Ломаем Timestamp каждого кластера (ID 0xE7 -> 0xEC, Void): время кластера не читается.
  const broken = webm.bytes.slice();
  for (const start of webm.mediaStarts) broken[start + 12] = 0xec;
  const { journal: webmJournal } = newJournal();
  addSourceBuffer(webmJournal, 0, "video/webm");
  append(webmJournal, 0, segmentsOf(broken, webm.initEnd, webm.mediaStarts));
  assert.equal(webmJournal.evictOldestIfWatchedBefore(1_000), false, "WebM cluster time is not readable");
  assert.deepEqual(replayedBytes(webmJournal, 0), broken, "the WebM journal is whole");

  const { budget, journal } = newJournal();
  addSourceBuffer(journal, 0, VIDEO_MIME);
  const chunks = segmentsOf(FILE, INIT_END, SEGMENT_STARTS).map((chunk) => chunk.slice());
  for (const chunk of chunks) journal.recordBytes(0, chunk);
  // Страница отдала буфер второго медиасегмента воркеру: его заголовок (время границы 1,2 с) больше не прочитать.
  const secondSegment = chunks[2];
  assert.ok(secondSegment !== undefined);
  structuredClone(secondSegment.buffer, { transfer: [secondSegment.buffer as ArrayBuffer] });
  // Уходит только запись с init (его держит дорожка, память та же): время границы 0 с ещё читается.
  assert.equal(evictWatched(journal, 1_000), 1);
  assert.equal(budget.memory.totalBytes, FILE.length, "the first media segment was not evicted on a guess");
});

test("watched part: an empty or incomplete journal has nothing to give", () => {
  const { journal: empty } = newJournal();
  addSourceBuffer(empty, 0, VIDEO_MIME);
  assert.equal(empty.evictOldestIfWatchedBefore(1_000), false);

  const { journal: incomplete } = newJournal();
  addSourceBuffer(incomplete, 0, VIDEO_MIME);
  append(incomplete, 0, segmentsOf(FILE, INIT_END, SEGMENT_STARTS));
  incomplete.markIncomplete();
  assert.equal(incomplete.evictOldestIfWatchedBefore(1_000), false);
  assert.deepEqual(replayedBytes(incomplete, 0), FILE.subarray(0, INIT_END), "the snapshot init is untouched");
});

test("a journal remembers that it was replayed: only then its video has gone to the player", () => {
  const { journal } = newJournal();
  addSourceBuffer(journal, 0, VIDEO_MIME);
  append(journal, 0, segmentsOf(FILE, INIT_END, SEGMENT_STARTS));
  assert.equal(journal.hasBeenReplayed, false);
  evictWatched(journal, 3);
  assert.equal(journal.hasBeenReplayed, false, "eviction is not a replay");
  journal.replay();
  assert.equal(journal.hasBeenReplayed, true);
});
