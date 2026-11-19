// Кто отдаёт память первым, когда документ выше потолка «памяти без копий» (решение 364): сначала
// просмотренное, начала ещё не игравших роликов и место вокруг текущей секунды — последними.
//
// Проверяется итог, который видит плеер: что именно журнал отдаст в повторе (с какой границы сегмента
// он начнётся). Каждый тест, кроме помеченных «страховка», падает на правиле «первым отдаёт самый
// старый MediaSource» (решение 363): в них самый старый MediaSource — как раз то, что нужно сберечь.
//
// Фикстура — ролик 6 с из пяти сегментов по 1,2 с. Длинный ролик собирается из неё законным приёмом
// MSE: страница двигает `timestampOffset` на длину ролика и кладёт те же сегменты ещё раз.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { JournalViewing } from "../../src/page/journal/journal-viewing.ts";
import { VIEWING_UNKNOWN } from "../../src/page/journal/journal-viewing.ts";
import { DocumentMemoryBudget, WATCHED_KEEP_BEHIND_SECONDS } from "../../src/page/journal/memory-budget.ts";
import type { MediaJournal, ReplayItem } from "../../src/page/journal/media-journal.ts";
import { VIDEO_64_6S_MP4, concat } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
/** Границы медиасегментов фикстуры и конец файла. */
const SEGMENT_EDGES: readonly number[] = [...VIDEO_64_6S_MP4.mediaStarts, FILE.length];
/** Сколько секунд длится фикстура и один её сегмент (шкала 12800, фрагменты по 15360 единиц). */
const FILE_SECONDS = 6;
const SEGMENT_SECONDS = 1.2;
/** Начало ролика, которое страница грузит заранее: init и первый сегмент. */
const START_BYTES = SEGMENT_EDGES[1] ?? 0;
const START = FILE.subarray(0, START_BYTES);
const INIT = FILE.subarray(0, INIT_END);

/**
 * Пройденный ролик, элемента которого в документе нет. До решения 377 у пройденного места не было; ролик вне
 * документа и сейчас отдаёт первым (ступень 1), поэтому прежние тесты ступени 1 смотрят именно на такой.
 */
const PASSED: JournalViewing = { kind: "passed", place: { kind: "off_page" } };
const IN_PLAYER: JournalViewing = { kind: "in_player" };
const OFF_PAGE: JournalViewing = { kind: "not_started", place: { kind: "off_page" } };

function playingAt(playheadSeconds: number): JournalViewing {
  return { kind: "watching_on_page", playheadSeconds, playback: "playing" };
}

function pausedAt(playheadSeconds: number): JournalViewing {
  return { kind: "watching_on_page", playheadSeconds, playback: "paused" };
}

function notStartedAt(distance: number): JournalViewing {
  return { kind: "not_started", place: { kind: "on_page", distanceFromView: () => distance } };
}

/** Видео документа в тесте: журнал и то, что «владелец» отвечает о его просмотре (можно менять по ходу). */
interface Reel {
  readonly journal: MediaJournal;
  viewing: JournalViewing;
  gone: boolean;
  /** Сколько раз потолок спросил о просмотре. */
  viewingReads: number;
}

function openReel(budget: DocumentMemoryBudget, key: string, viewing: JournalViewing): Reel {
  const reel: Reel = {
    viewing,
    gone: false,
    viewingReads: 0,
    journal: budget.createJournal(key, {
      isGone: () => reel.gone,
      viewing: () => {
        reel.viewingReads += 1;
        return reel.viewing;
      },
    }),
  };
  reel.journal.recordEvent({ kind: "source_buffer_added", media_source: key, source_buffer: 0, mime_type: VIDEO_MIME });
  return reel;
}

/** Страница кладёт кусок файла отдельной загрузкой (свой буфер), и потолок наводит порядок. */
function download(budget: DocumentMemoryBudget, reel: Reel, start: number, end: number): void {
  reel.journal.recordBytes(0, FILE.slice(start, end));
  budget.enforce();
}

function downloadStart(budget: DocumentMemoryBudget, reel: Reel): void {
  download(budget, reel, 0, INIT_END);
  download(budget, reel, INIT_END, START_BYTES);
}

/** Сегменты `from..to` (номера с нуля) по одному на загрузку. */
function downloadSegments(budget: DocumentMemoryBudget, reel: Reel, from: number, to: number): void {
  for (let segment = from; segment < to; segment += 1) download(budget, reel, SEGMENT_EDGES[segment] ?? 0, SEGMENT_EDGES[segment + 1] ?? 0);
}

function downloadWholeFile(budget: DocumentMemoryBudget, reel: Reel): void {
  download(budget, reel, 0, INIT_END);
  downloadSegments(budget, reel, 0, SEGMENT_EDGES.length - 1);
}

/** Ролик длиной `repeats` × 6 с: фикстура, повторённая со сдвигом `timestampOffset`. */
function downloadLongReel(budget: DocumentMemoryBudget, reel: Reel, repeats: number): void {
  download(budget, reel, 0, INIT_END);
  for (let repeat = 0; repeat < repeats; repeat += 1) {
    reel.journal.recordEvent({
      kind: "timestamp_offset_changed",
      media_source: reel.journal.key,
      source_buffer: 0,
      offset_us: repeat * FILE_SECONDS * 1_000_000,
    });
    downloadSegments(budget, reel, 0, SEGMENT_EDGES.length - 1);
  }
}

function replayedBytes(journal: MediaJournal): Uint8Array {
  const items: readonly ReplayItem[] = journal.replay();
  return concat(items.flatMap((item) => (item.kind === "bytes" ? [item.view] : [])));
}

/** Сколько медиасегментов осталось в повторе (сегменты фикстуры разной длины, поэтому считаем по байтам с конца). */
function mediaBytesKept(journal: MediaJournal): number {
  return replayedBytes(journal).length - INIT_END;
}

/** Повтор начинается с самого начала ролика: init и первый сегмент на месте. */
function assertStartsAtZero(reel: Reel, what: string): void {
  assert.deepEqual(replayedBytes(reel.journal).subarray(0, START_BYTES), START, `${what}: replay starts at 0 with init and the first segment`);
}

function assertOnlyInitLeft(reel: Reel, what: string): void {
  assert.deepEqual(replayedBytes(reel.journal), INIT, `${what}: gave up all its media, the init stays`);
}

test("feed after the page dropped the passed reels: far preloads give their starts, the playing reel and the next one stay whole", () => {
  // Влезают текущий ролик целиком, начало следующего и начало ОДНОГО дальнего (плюс init двух других).
  const cap = FILE.length + 2 * START_BYTES + 2 * INIT_END + 1_000;
  const budget = new DocumentMemoryBudget(cap);
  // Пройденные ролики страница уже сбросила: самый старый живой MediaSource — текущий, за ним следующий.
  const playing = openReel(budget, "ms-1", playingAt(0.5));
  const next = openReel(budget, "ms-2", notStartedAt(800));
  const farReels = ["ms-3", "ms-4", "ms-5"].map((key) => openReel(budget, key, OFF_PAGE));
  // Страница качает начала по кругу, потом докачивает текущий ролик: документ переходит потолок.
  for (const reel of [playing, next, ...farReels]) downloadStart(budget, reel);
  downloadSegments(budget, playing, 1, SEGMENT_EDGES.length - 1);

  assert.ok(budget.memory.totalBytes <= cap, "the document is back under the cap");
  assert.deepEqual(replayedBytes(playing.journal), FILE, "the playing reel is whole: a hand-off at its playhead finds data");
  assert.deepEqual(replayedBytes(next.journal), START, "the next reel keeps its start");
  // Память отдали два самых новых дальних ролика, по одному: третий цел.
  const [nearestFar, middleFar, newestFar] = farReels;
  assert.ok(nearestFar !== undefined && middleFar !== undefined && newestFar !== undefined);
  assertOnlyInitLeft(newestFar, "the newest far reel");
  assertOnlyInitLeft(middleFar, "the second newest far reel");
  assertStartsAtZero(nearestFar, "the far reel that is next in the feed");
});

test("a passed reel gives everything before the reel on the page gives anything, even when the passed one is newer", () => {
  const cap = FILE.length + INIT_END + 1_000;
  const budget = new DocumentMemoryBudget(cap);
  const playing = openReel(budget, "ms-1", playingAt(0.5));
  const passed = openReel(budget, "ms-2", PASSED);
  downloadWholeFile(budget, passed);
  downloadWholeFile(budget, playing);

  assert.ok(budget.memory.totalBytes <= cap);
  assert.deepEqual(replayedBytes(playing.journal), FILE);
  assertOnlyInitLeft(passed, "the passed reel");
});

test("the watched part goes first and stops 10 s behind the playhead; then far reels give, the next reel does not", () => {
  // Ролик 18 с, страница стоит на 14-й секунде: просмотренным считается всё до 4-й (14 − 10).
  const playheadSeconds = 14;
  // Влезают ролик без трёх первых сегментов и начало одного ролика; второму началу места нет.
  const longReelBytes = INIT_END + 3 * (FILE.length - INIT_END);
  const watchedBytes = (SEGMENT_EDGES[3] ?? 0) - INIT_END;
  const cap = longReelBytes - watchedBytes + START_BYTES + INIT_END + 500;
  const budget = new DocumentMemoryBudget(cap);
  const film = openReel(budget, "ms-1", playingAt(playheadSeconds));
  const next = openReel(budget, "ms-2", notStartedAt(700));
  const far = openReel(budget, "ms-3", notStartedAt(5_000));
  downloadStart(budget, next);
  downloadStart(budget, far);
  downloadLongReel(budget, film, 3);

  assert.ok(budget.memory.totalBytes <= cap);
  // Повтор начинается с сегмента 3,6 с: следующая граница (4,8 с) уже позже 4-й секунды.
  const keptFrom = SEGMENT_EDGES[3] ?? 0;
  const expectedMedia = concat([FILE.subarray(keptFrom), FILE.subarray(INIT_END), FILE.subarray(INIT_END)]);
  assert.deepEqual(replayedBytes(film.journal), concat([INIT, expectedMedia]), "the reel gave exactly its first three segments");
  const replayStartSeconds = 3 * SEGMENT_SECONDS;
  assert.ok(replayStartSeconds <= playheadSeconds - WATCHED_KEEP_BEHIND_SECONDS, "the replay still starts 10 s before the playhead");
  // Просмотренного не хватило: дальше отдаёт дальний ролик, а следующий цел.
  assertOnlyInitLeft(far, "the far reel");
  assertStartsAtZero(next, "the next reel");
});

test("insurance: a single media source gives its own head as before and the page is not asked anything", () => {
  const cap = 12_000;
  const budget = new DocumentMemoryBudget(cap);
  const film = openReel(budget, "ms-1", playingAt(0.5));
  downloadWholeFile(budget, film);

  assert.ok(budget.memory.totalBytes <= cap);
  const bytes = replayedBytes(film.journal);
  const tailStart = FILE.length - (bytes.length - INIT_END);
  assert.ok(tailStart > INIT_END && SEGMENT_EDGES.includes(tailStart), `the film gave its head up to a segment boundary (${tailStart})`);
  assert.deepEqual(bytes, concat([INIT, FILE.subarray(tailStart)]));
  assert.equal(film.viewingReads, 0, "with one media source there is nothing to choose: the page is not read");
});

test("mid-roll ad: the film paused on screen keeps the area around its playhead, the playing ad gives its own head", () => {
  // Фильм 18 с стоит на паузе на 14-й секунде, поверх него играет реклама и докачивается сверх потолка.
  const longReelBytes = INIT_END + 3 * (FILE.length - INIT_END);
  const watchedBytes = (SEGMENT_EDGES[3] ?? 0) - INIT_END;
  // После просмотренной части фильма не хватает ещё двух сегментов рекламы.
  const adBytesToGive = (SEGMENT_EDGES[2] ?? 0) - INIT_END;
  const cap = longReelBytes - watchedBytes + FILE.length - adBytesToGive + 500;
  const budget = new DocumentMemoryBudget(cap);
  const film = openReel(budget, "ms-1", pausedAt(14));
  const ad = openReel(budget, "ms-2", playingAt(0.5));
  downloadLongReel(budget, film, 3);
  downloadWholeFile(budget, ad);

  assert.ok(budget.memory.totalBytes <= cap);
  // Фильм отдал только просмотренное: повтор с сегмента 3,6 с, передача с 14-й секунды найдёт данные.
  assert.equal(mediaBytesKept(film.journal), longReelBytes - INIT_END - watchedBytes, "the film gave its watched part and nothing more");
  // Остальное отдала играющая реклама: её потеря у текущей секунды проходит сама, пока она играет.
  assert.deepEqual(replayedBytes(ad.journal), concat([INIT, FILE.subarray(SEGMENT_EDGES[2] ?? 0)]));
});

test("pre-roll ad: the playing ad gives its head before the film that has not started loses its start", () => {
  const cap = FILE.length + START_BYTES - ((SEGMENT_EDGES[2] ?? 0) - INIT_END) + 500;
  const budget = new DocumentMemoryBudget(cap);
  // Фильм заведён раньше рекламы и ещё не играл: его начало потеряется навсегда, реклама свою потерю переживёт.
  const film = openReel(budget, "ms-1", notStartedAt(0));
  const ad = openReel(budget, "ms-2", playingAt(0.5));
  downloadStart(budget, film);
  downloadWholeFile(budget, ad);

  assert.ok(budget.memory.totalBytes <= cap);
  assert.deepEqual(replayedBytes(film.journal), START, "the film keeps its start");
  assert.deepEqual(replayedBytes(ad.journal), concat([INIT, FILE.subarray(SEGMENT_EDGES[2] ?? 0)]));
});

test("the video already playing in the player gives before any start; the nearest not-started reel is the very last", () => {
  const cap = 2 * START_BYTES + 2 * INIT_END + 500;
  const budget = new DocumentMemoryBudget(cap);
  // Самый старый MediaSource — следующий ролик ленты: его начало и надо сберечь.
  const next = openReel(budget, "ms-1", notStartedAt(700));
  const inPlayer = openReel(budget, "ms-2", IN_PLAYER);
  const far = openReel(budget, "ms-3", notStartedAt(4_000));
  const farther = openReel(budget, "ms-4", notStartedAt(9_000));
  downloadStart(budget, next);
  downloadStart(budget, far);
  downloadStart(budget, farther);
  downloadWholeFile(budget, inPlayer);

  assert.ok(budget.memory.totalBytes <= cap);
  assertOnlyInitLeft(inPlayer, "the video in the player (the player already has its data)");
  assertOnlyInitLeft(farther, "the farthest not-started reel");
  assertStartsAtZero(far, "the nearer of the far reels");
  assertStartsAtZero(next, "the next reel");
});

/** Потолок, под который влезают начала всех роликов, кроме одного: ровно один обязан отдать своё. */
function capForAllStartsButOne(reelCount: number): number {
  return (reelCount - 1) * START_BYTES + INIT_END + 500;
}

/** Ролик, который ещё не играл, со счётчиком: сколько раз мерили его место на странице. */
interface MeasuredReel {
  readonly reel: Reel;
  distanceReads: number;
}

function openMeasuredReel(budget: DocumentMemoryBudget, key: string, distance: number): MeasuredReel {
  const viewing: JournalViewing = {
    kind: "not_started",
    place: {
      kind: "on_page",
      distanceFromView: () => {
        measured.distanceReads += 1;
        return distance;
      },
    },
  };
  const measured: MeasuredReel = { reel: openReel(budget, key, viewing), distanceReads: 0 };
  return measured;
}

test("not-started reels on the page: the farthest from the view gives its start, not the oldest", () => {
  const budget = new DocumentMemoryBudget(capForAllStartsButOne(3));
  const nearest = openReel(budget, "ms-1", notStartedAt(50));
  const farthest = openReel(budget, "ms-2", notStartedAt(9_000));
  const middle = openReel(budget, "ms-3", notStartedAt(400));
  for (const reel of [nearest, farthest, middle]) downloadStart(budget, reel);

  assertOnlyInitLeft(farthest, "the farthest reel");
  assertStartsAtZero(nearest, "the nearest reel");
  assertStartsAtZero(middle, "the reel in the middle");
});

test("of two equally far not-started reels the newest gives: the page opens media sources along the feed", () => {
  const budget = new DocumentMemoryBudget(capForAllStartsButOne(4));
  const olderFar = openReel(budget, "ms-1", notStartedAt(900));
  const nearest = openReel(budget, "ms-2", notStartedAt(50));
  const middle = openReel(budget, "ms-3", notStartedAt(400));
  const newerFar = openReel(budget, "ms-4", notStartedAt(900));
  // Все четыре начала уже лежат, когда потолок выбирает: кладём без наведения порядка, потом наводим.
  for (const reel of [olderFar, nearest, middle, newerFar]) {
    reel.journal.recordBytes(0, FILE.slice(0, INIT_END));
    reel.journal.recordBytes(0, FILE.slice(INIT_END, START_BYTES));
  }
  budget.enforce();

  assertOnlyInitLeft(newerFar, "the newer of the two farthest reels");
  for (const reel of [olderFar, nearest, middle]) assertStartsAtZero(reel, `reel ${reel.journal.key}`);
});

test("a not-started reel outside the document gives before any reel on the page, and the page layout is not measured for that", () => {
  const budget = new DocumentMemoryBudget(capForAllStartsButOne(3));
  const farOnPage = openMeasuredReel(budget, "ms-1", 9_000);
  const offPage = openReel(budget, "ms-2", OFF_PAGE);
  const nearOnPage = openMeasuredReel(budget, "ms-3", 50);
  for (const reel of [farOnPage.reel, offPage, nearOnPage.reel]) downloadStart(budget, reel);

  assertOnlyInitLeft(offPage, "the reel whose element is not in the document");
  assertStartsAtZero(farOnPage.reel, "the far reel on the page");
  assertStartsAtZero(nearOnPage.reel, "the near reel on the page");
  assert.equal(farOnPage.distanceReads + nearOnPage.distanceReads, 0, "no layout read was needed");
});

test("the reel chosen to give keeps giving while it has something: the page layout is not measured on every appendBuffer", () => {
  // Влезают дальний ролик целиком и два начала; дальше следующий ролик докачивается сегмент за сегментом.
  const cap = FILE.length + 2 * START_BYTES + 500;
  const budget = new DocumentMemoryBudget(cap);
  const next = openMeasuredReel(budget, "ms-1", 700);
  const far = openMeasuredReel(budget, "ms-2", 3_000);
  const farthest = openMeasuredReel(budget, "ms-3", 6_000);
  const distanceReads = (): number => next.distanceReads + far.distanceReads + farthest.distanceReads;
  downloadStart(budget, next.reel);
  downloadStart(budget, far.reel);
  downloadWholeFile(budget, farthest.reel);
  // Первая нехватка памяти: потолок выбирает, кто отдаёт, и для этого меряет страницу.
  downloadSegments(budget, next.reel, 1, 2);
  const readsAfterTheChoice = distanceReads();
  assert.ok(readsAfterTheChoice > 0, "the first choice measured the page");
  const keptAfterTheChoice = mediaBytesKept(farthest.reel.journal);
  // Ещё три загрузки страницы сверх потолка.
  downloadSegments(budget, next.reel, 2, SEGMENT_EDGES.length - 1);

  assert.ok(budget.memory.totalBytes <= cap);
  assert.ok(mediaBytesKept(farthest.reel.journal) < keptAfterTheChoice, "the same reel kept giving");
  assertStartsAtZero(far.reel, "the other far reel");
  assert.deepEqual(replayedBytes(next.reel.journal), FILE, "the next reel is whole");
  assert.equal(distanceReads(), readsAfterTheChoice, "no layout read after the choice");
});

test("closed media sources are forgotten before any live one gives, without waiting for a new media source", () => {
  const cap = FILE.length + 1_000;
  const budget = new DocumentMemoryBudget(cap);
  const live = openReel(budget, "ms-1", notStartedAt(700));
  // По просмотру закрытый ролик — самый ценный (ближайший, ещё не игравший): живой он бы память не отдал.
  const dropped = openReel(budget, "ms-2", notStartedAt(5));
  downloadStart(budget, dropped);
  // Страница сняла источник с элемента: MediaSource закрыт, нового страница не заводила.
  dropped.gone = true;
  downloadWholeFile(budget, live);

  assert.deepEqual(replayedBytes(live.journal), FILE, "the live media source gave nothing");
  assert.equal(budget.memory.totalBytes, FILE.length, "everything of the closed media source is released, init included");
});

test("nothing known about viewing (worker): the oldest media source gives first, as before decision 364", () => {
  const cap = FILE.length + INIT_END + 1_000;
  const budget = new DocumentMemoryBudget(cap);
  const older = openReel(budget, "ms-1", VIEWING_UNKNOWN);
  const newer = openReel(budget, "ms-2", VIEWING_UNKNOWN);
  downloadWholeFile(budget, newer);
  downloadWholeFile(budget, older);

  assert.ok(budget.memory.totalBytes <= cap);
  assertOnlyInitLeft(older, "the older media source");
  assert.deepEqual(replayedBytes(newer.journal), FILE);
});

test("the owner is asked about viewing once per enforce, however many entries have to go", () => {
  const cap = START_BYTES + 2 * INIT_END + 500;
  const budget = new DocumentMemoryBudget(cap);
  const passed = openReel(budget, "ms-1", PASSED);
  const next = openReel(budget, "ms-2", notStartedAt(700));
  downloadStart(budget, next);
  // Пять сегментов пройденного ролика одной большой загрузкой не бывает: кладём по одному, без потолка.
  passed.journal.recordBytes(0, FILE.slice(0, INIT_END));
  for (let segment = 0; segment < SEGMENT_EDGES.length - 1; segment += 1) {
    passed.journal.recordBytes(0, FILE.slice(SEGMENT_EDGES[segment], SEGMENT_EDGES[segment + 1]));
  }
  passed.viewingReads = 0;
  next.viewingReads = 0;
  budget.enforce();

  assertOnlyInitLeft(passed, "the passed reel");
  assert.ok(passed.viewingReads === 1 && next.viewingReads <= 1, `viewing was read ${passed.viewingReads} and ${next.viewingReads} time(s)`);
});

// ---------------------------------------------------------------------------------------------------
// Ближайший пролистанный ролик позади (16b3b, решение 377): бережётся до ступени 7, как начало следующего.
// ---------------------------------------------------------------------------------------------------

/** Пройденный ролик в документе на расстоянии `distance` от окна; `measured` считает, сколько раз мерили. */
function passedOnPage(distance: number, measured: { count: number } = { count: 0 }): JournalViewing {
  return {
    kind: "passed",
    place: {
      kind: "on_page",
      distanceFromView: () => {
        measured.count += 1;
        return distance;
      },
    },
  };
}

test("the reel just scrolled past (in the document) gives after the far not-started reels, not first (377)", () => {
  // Влезают пролистанный ролик целиком, начало следующего и init дальнего.
  const cap = FILE.length + START_BYTES + INIT_END + 500;
  const budget = new DocumentMemoryBudget(cap);
  const measured = { count: 0 };
  const behind = openReel(budget, "ms-1", passedOnPage(740, measured));
  const next = openReel(budget, "ms-2", notStartedAt(740));
  const far = openReel(budget, "ms-3", notStartedAt(2_220));
  downloadWholeFile(budget, behind);
  downloadStart(budget, next);
  downloadWholeFile(budget, far);

  assert.ok(budget.memory.totalBytes <= cap);
  assert.deepEqual(replayedBytes(behind.journal), FILE, "the reel one scroll back is whole: going back finds its data");
  assert.deepEqual(replayedBytes(next.journal), START, "the next reel keeps its start");
  assertOnlyInitLeft(far, "the far reel ahead");
  assert.equal(measured.count, 0, "a single passed reel on the page is the nearest one without measuring the layout");
});

test("two passed reels on the page: the farther one gives first, whatever the creation order (377)", () => {
  for (const nearIsOlder of [true, false]) {
    const cap = FILE.length + START_BYTES + INIT_END + 500;
    const budget = new DocumentMemoryBudget(cap);
    const first = openReel(budget, "ms-1", passedOnPage(nearIsOlder ? 740 : 1_480));
    const second = openReel(budget, "ms-2", passedOnPage(nearIsOlder ? 1_480 : 740));
    const [near, farBehind] = nearIsOlder ? [first, second] : [second, first];
    const next = openReel(budget, "ms-3", notStartedAt(740));
    downloadWholeFile(budget, near);
    downloadWholeFile(budget, farBehind);
    downloadStart(budget, next);

    assert.ok(budget.memory.totalBytes <= cap);
    assertOnlyInitLeft(farBehind, `the reel two scrolls back (near is older: ${nearIsOlder})`);
    assert.deepEqual(replayedBytes(near.journal), FILE, `the reel one scroll back is whole (near is older: ${nearIsOlder})`);
    assert.deepEqual(replayedBytes(next.journal), START);
  }
});

test("only one reel behind is kept: when the nearest one has nothing left, the next passed reel still gives first (377)", () => {
  const cap = START_BYTES + 3 * INIT_END + 500;
  const budget = new DocumentMemoryBudget(cap);
  // Ближайший позади уже отдал всё (страница качала его одним init); бережётся он, а не следующий за ним.
  const near = openReel(budget, "ms-1", passedOnPage(740));
  const farBehind = openReel(budget, "ms-2", passedOnPage(1_480));
  const next = openReel(budget, "ms-3", notStartedAt(740));
  download(budget, near, 0, INIT_END);
  downloadWholeFile(budget, farBehind);
  downloadStart(budget, next);

  assertOnlyInitLeft(farBehind, "the reel two scrolls back");
  assert.deepEqual(replayedBytes(next.journal), START, "the next reel keeps its start: the protection did not move to the older reel");
});
