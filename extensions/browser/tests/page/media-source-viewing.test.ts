// Свидетель просмотра (решение 364): что он отвечает потолку памяти о видео каждого MediaSource.
//
// Страница здесь — набор фейковых элементов с настоящими для свидетеля признаками: источник, пауза,
// «сыгранное», место на экране, в документе ли элемент и стоит ли на нём тень. Журнал — настоящий
// (свидетель читает у него только «повтор уже запрашивали»).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { JournalViewing } from "../../src/page/journal/journal-viewing.ts";
import { DocumentMemoryBudget } from "../../src/page/journal/memory-budget.ts";
import { VIEWING_UNKNOWN } from "../../src/page/journal/journal-viewing.ts";
import { ELEMENT_SEARCH_INTERVAL_MS, MediaSourceViewing } from "../../src/page/media-source-viewing.ts";
import { MseTracker, type MediaSourceRecord } from "../../src/page/mse-tracker.ts";
import { VIDEO_64_6S_MP4, concat } from "./support/media-samples.ts";

/** Окно страницы теста. */
const VIEWPORT = { width: 1000, height: 800 };
/** Прямоугольник ролика, стоящего по центру окна, и ролика на экран ниже. */
const ON_SCREEN = { left: 300, top: 200, right: 700, bottom: 600 };
const ONE_SCREEN_BELOW = { left: 300, top: 1000, right: 700, bottom: 1400 };
/** Пролистанный ролик живого Instagram: от него сверху окна осталась полоска в пятую часть площади. */
const STRIP_AT_THE_TOP = { left: 300, top: -320, right: 700, bottom: 80 };

/** Фейковый `<video>`: только то, что читает свидетель. */
interface FakeVideo {
  src: string;
  inDocument: boolean;
  paused: boolean;
  ended: boolean;
  hasPlayed: boolean;
  currentTime: number;
  rect: { left: number; top: number; right: number; bottom: number };
  /** Элемент под тенью: его видео идёт (или вот-вот пойдёт) в окне плеера. */
  shadowed: boolean;
  /** Идёт передача этого элемента (повтор журнала плеер получил в её начале). */
  handedOff: boolean;
}

interface Page {
  readonly viewing: MediaSourceViewing;
  readonly videos: FakeVideo[];
  nowMs: number;
  documentSearches: number;
  visibleShareReads: number;
  /** Новый MediaSource документа: запись с журналом; `objectUrl` — его blob-адрес. */
  openMediaSource(objectUrl: string | null): MediaSourceRecord;
  /** Новый `<video>` с этим источником. */
  addVideo(src: string, overrides?: Partial<FakeVideo>): FakeVideo;
}

function createPage(): Page {
  const budget = new DocumentMemoryBudget();
  let lastKey = 0;
  const asVideo = (element: object): FakeVideo => element as FakeVideo;
  const visibleShareOf = (video: FakeVideo): number => {
    if (!video.inDocument) return 0;
    const width = Math.max(0, Math.min(video.rect.right, VIEWPORT.width) - Math.max(video.rect.left, 0));
    const height = Math.max(0, Math.min(video.rect.bottom, VIEWPORT.height) - Math.max(video.rect.top, 0));
    return (width * height) / ((video.rect.right - video.rect.left) * (video.rect.bottom - video.rect.top));
  };
  const page: Page = {
    videos: [],
    nowMs: 0,
    documentSearches: 0,
    visibleShareReads: 0,
    viewing: new MediaSourceViewing({
      dom: {
        isConnected: (element) => asVideo(element).inDocument,
        isPaused: (element) => asVideo(element).paused,
        isEnded: (element) => asVideo(element).ended,
        currentTimeSeconds: (element) => asVideo(element).currentTime,
        hasPlayed: (element) => asVideo(element).hasPlayed,
        mediaSourceUrlsOf: (element) => [asVideo(element).src],
        rectOf: (element) => asVideo(element).rect,
        viewportSize: () => VIEWPORT,
        videoElementsInDocument: () => {
          page.documentSearches += 1;
          return page.videos.filter((video) => video.inDocument) as unknown as Element[];
        },
      },
      visibleShareOf: (element) => {
        page.visibleShareReads += 1;
        return visibleShareOf(asVideo(element));
      },
      isHandedOff: (element) => asVideo(element).handedOff,
      isUnderShadow: (element) => asVideo(element).shadowed,
      now: () => page.nowMs,
    }),
    openMediaSource: (objectUrl) => {
      lastKey += 1;
      const key = `ms-${lastKey}`;
      const journal = budget.createJournal(key, { isGone: () => false, viewing: () => VIEWING_UNKNOWN });
      page.viewing.noteMediaSourceStarted();
      return { key, journal, objectUrl, mediaSource: new WeakRef({}) };
    },
    addVideo: (src, overrides = {}) => {
      const video: FakeVideo = { src, inDocument: true, paused: true, ended: false, hasPlayed: false, currentTime: 0, rect: ON_SCREEN, shadowed: false, handedOff: false, ...overrides };
      page.videos.push(video);
      return video;
    },
  };
  return page;
}

/** Ролик ленты: MediaSource и его `<video>` в документе. */
function addReel(page: Page, url: string, overrides: Partial<FakeVideo> = {}): { record: MediaSourceRecord; video: FakeVideo } {
  return { record: page.openMediaSource(url), video: page.addVideo(url, overrides) };
}

/** Страница запустила ролик: он играет с этой секунды. */
function play(video: FakeVideo, currentTime: number): void {
  Object.assign(video, { paused: false, hasPlayed: true, currentTime });
}

/**
 * Где пройденный ролик (решение 377): в документе — расстояние от окна, вне документа — `off_page`; не пройден —
 * вид просмотра. Расстояние потолку нужно, чтобы беречь ближайший пролистанный ролик позади.
 */
function passedPlaceOf(viewing: JournalViewing): number | "off_page" | JournalViewing["kind"] {
  if (viewing.kind !== "passed") return viewing.kind;
  return viewing.place.kind === "off_page" ? "off_page" : viewing.place.distanceFromView();
}

function distanceOf(viewing: JournalViewing): number | "off_page" | JournalViewing["kind"] {
  if (viewing.kind !== "not_started") return viewing.kind;
  return viewing.place.kind === "off_page" ? "off_page" : viewing.place.distanceFromView();
}

test("a reel that has not played yet is measured from the view; an element never found in the document is unknown, not 'not started'", () => {
  const page = createPage();
  const centred = addReel(page, "blob:centred");
  const below = addReel(page, "blob:below", { rect: ONE_SCREEN_BELOW });
  // Элемент вне документа с самого начала или внутри shadow root: поиск по документу его не видит.
  const neverInTheDocument = addReel(page, "blob:outside", { inDocument: false });
  const neverAttached = page.openMediaSource("blob:nobody-has-it");

  assert.equal(distanceOf(page.viewing.viewingOf(centred.record)), 0, "a video in the middle of the window");
  assert.equal(distanceOf(page.viewing.viewingOf(below.record)), 800, "a video one screen below");
  assert.deepEqual(page.viewing.viewingOf(neverInTheDocument.record), VIEWING_UNKNOWN);
  assert.deepEqual(page.viewing.viewingOf(neverAttached), VIEWING_UNKNOWN);
  // Скрытый элемент без прямоугольника — бесконечно далеко, а не «в углу окна».
  const hidden = addReel(page, "blob:hidden", { rect: { left: 0, top: 0, right: 0, bottom: 0 } });
  assert.equal(distanceOf(page.viewing.viewingOf(hidden.record)), Number.POSITIVE_INFINITY);
});

test("a media source without a blob address cannot be matched to an element: nothing is known about it", () => {
  const page = createPage();
  assert.deepEqual(page.viewing.viewingOf(page.openMediaSource(null)), VIEWING_UNKNOWN);
});

test("the reel the page plays is being watched at the page position; paused on screen it still is", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  play(reel.video, 12.5);
  assert.deepEqual(page.viewing.viewingOf(reel.record), { kind: "watching_on_page", playheadSeconds: 12.5, playback: "playing" });

  // Зритель поставил паузу: ролик на экране, к этой секунде вернутся.
  reel.video.paused = true;
  assert.deepEqual(page.viewing.viewingOf(reel.record), { kind: "watching_on_page", playheadSeconds: 12.5, playback: "paused" });
});

test("a reel that played and then left the screen paused is passed, and stays passed until it plays again", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  play(reel.video, 7);
  page.viewing.viewingOf(reel.record);
  // Лента ушла дальше: страница остановила ролик, он за экраном. Позицию страница сбросила на ноль.
  Object.assign(reel.video, { paused: true, currentTime: 0, rect: ONE_SCREEN_BELOW });
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), 800);

  // Долю пройденного ролика на экране больше не меряем. Расстояние от окна (решение 377) читается лениво — только
  // когда потолок его спросит, сравнивая пройденные ролики в документе.
  const readsWhenPassed = page.visibleShareReads;
  reel.video.rect = ON_SCREEN;
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), 0);
  assert.equal(page.visibleShareReads, readsWhenPassed);
  // Зритель вернулся и ролик заиграл: его снова смотрят.
  play(reel.video, 3);
  assert.deepEqual(page.viewing.viewingOf(reel.record), { kind: "watching_on_page", playheadSeconds: 3, playback: "playing" });
});

test("a paused reel of which only a strip is left on screen is passed: a strip is not watching", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  play(reel.video, 30);
  page.viewing.viewingOf(reel.record);
  // Лента переключилась на полпути: прежний ролик остановлен и выглядывает полоской до следующей прокрутки.
  Object.assign(reel.video, { paused: true, rect: STRIP_AT_THE_TOP });
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), 520);
});

test("a played reel the page removed from the document is passed; one that never played is not", () => {
  const page = createPage();
  const played = addReel(page, "blob:played");
  const preloaded = addReel(page, "blob:preloaded");
  play(played.video, 20);
  page.viewing.viewingOf(played.record);
  page.viewing.viewingOf(preloaded.record);
  // Страница убрала оба элемента из документа (пройденный — насовсем, заранее загруженный — в запас).
  Object.assign(played.video, { paused: true, inDocument: false });
  preloaded.video.inDocument = false;

  assert.equal(passedPlaceOf(page.viewing.viewingOf(played.record)), "off_page");
  assert.equal(distanceOf(page.viewing.viewingOf(preloaded.record)), "off_page", "no evidence of watching, so not passed");
});

/** Передача элемента началась: тень стоит, плеер получил повтор журнала. */
function handOff(reel: { record: MediaSourceRecord; video: FakeVideo }): void {
  Object.assign(reel.video, { shadowed: true, handedOff: true });
  reel.record.journal.replay();
}

/** Передача кончилась переездом окна: тень снята, настоящий элемент стоит на нуле. */
function leaveForTheNextReel(video: FakeVideo): void {
  Object.assign(video, { shadowed: false, handedOff: false, paused: true, currentTime: 0, rect: ONE_SCREEN_BELOW });
}

test("a held video about to go to the player is the nearest not-started one; once handed off it is in the player", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel", { rect: ONE_SCREEN_BELOW });
  // Сторож задержал ролик для плеера: тень стоит, настоящий элемент на паузе, повтора ещё не было.
  reel.video.shadowed = true;
  assert.equal(distanceOf(page.viewing.viewingOf(reel.record)), 0, "nothing is nearer to being shown");
  handOff(reel);
  assert.deepEqual(page.viewing.viewingOf(reel.record), { kind: "in_player" });
});

test("a reel that was in the player before and is held again is not 'in the player' until the new hand-off begins", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  handOff(reel);
  leaveForTheNextReel(reel.video);
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), 800);
  // Зритель листнул назад: страница снова запускает ролик, сторож задержал его для плеера. Повтор журнала
  // ещё впереди, и отдавать его память сейчас — значит оставить плеер без начала.
  Object.assign(reel.video, { shadowed: true, rect: ON_SCREEN });
  assert.equal(distanceOf(page.viewing.viewingOf(reel.record)), 0);
});

test("a reel watched only in the player is passed once the feed moved on, although its real element never played", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  handOff(reel);
  assert.deepEqual(page.viewing.viewingOf(reel.record), { kind: "in_player" });
  // Окно переехало на следующий ролик: тень снята, настоящий элемент стоит на нуле и ушёл с экрана.
  leaveForTheNextReel(reel.video);
  assert.equal(reel.video.hasPlayed, false, "the real element has no played ranges");
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), 800);
});

test("a replayed reel is passed even if nobody asked about it while it was in the player", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  reel.record.journal.replay();
  // Пока ролик шёл в плеере, документ был под потолком: свидетеля не спрашивали ни разу.
  Object.assign(reel.video, { paused: true, currentTime: 0, rect: ONE_SCREEN_BELOW });
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), 800);
});

test("the element is remembered: its state is read after it left the document, and a new source on it is not ours", () => {
  const page = createPage();
  const reel = addReel(page, "blob:reel");
  play(reel.video, 4);
  page.viewing.viewingOf(reel.record);
  const searchesToFind = page.documentSearches;
  reel.video.inDocument = false;
  // Элемент вне документа ещё играет (звук идёт): обход документа его бы не нашёл, а он всё ещё «смотрится».
  assert.deepEqual(page.viewing.viewingOf(reel.record), { kind: "watching_on_page", playheadSeconds: 4, playback: "playing" });
  // Страница остановила его: вне документа и на паузе — пройден.
  reel.video.paused = true;
  assert.equal(passedPlaceOf(page.viewing.viewingOf(reel.record)), "off_page");
  assert.equal(page.documentSearches, searchesToFind, "the remembered element needs no document search");

  // Один `<video>` на всю ленту: страница дала ему новый MediaSource, прежний с ним больше не связан.
  const shared = addReel(page, "blob:first");
  play(shared.video, 9);
  assert.equal(page.viewing.viewingOf(shared.record).kind, "watching_on_page");
  shared.video.src = "blob:second";
  page.nowMs += ELEMENT_SEARCH_INTERVAL_MS;
  assert.equal(passedPlaceOf(page.viewing.viewingOf(shared.record)), "off_page", "it was watched and its element is gone");
});

test("the document is searched at once for a new media source, and at most once per interval for one that is not found", () => {
  const page = createPage();
  const lost = page.openMediaSource("blob:not-in-the-document-yet");
  assert.deepEqual(page.viewing.viewingOf(lost), VIEWING_UNKNOWN);
  assert.equal(page.documentSearches, 1);
  // Страница качает дальше, свидетеля спрашивают на каждый `appendBuffer`: документ заново не ищется.
  for (let ask = 0; ask < 50; ask += 1) {
    page.nowMs += 10;
    page.viewing.viewingOf(lost);
  }
  assert.equal(page.documentSearches, 1, "50 questions within half a second: no new search");

  // Элемент появился в документе: его находит обход после конца интервала.
  const video = page.addVideo("blob:not-in-the-document-yet");
  page.nowMs += ELEMENT_SEARCH_INTERVAL_MS;
  assert.equal(distanceOf(page.viewing.viewingOf(lost)), 0);
  assert.equal(page.documentSearches, 2);
  assert.equal(video.inDocument, true);

  // Новый MediaSource ищется сразу, не дожидаясь интервала: «не найден» значило бы «дальше всех».
  const fresh = addReel(page, "blob:fresh");
  assert.equal(distanceOf(page.viewing.viewingOf(fresh.record)), 0);
  assert.equal(page.documentSearches, 3);
  // И найденное по дороге больше не ищется.
  page.viewing.viewingOf(fresh.record);
  page.viewing.viewingOf(lost);
  assert.equal(page.documentSearches, 3);
});

test("a page read that throws inside appendBuffer does not reach the page: the media source is treated as unknown and memory is still enforced", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const oddities: string[] = [];
  const cap = file.length + VIDEO_64_6S_MP4.initEnd + 1_000;
  const budget = new DocumentMemoryBudget(cap);
  const records: MediaSourceRecord[] = [];
  const tracker = new MseTracker({
    budget,
    readyStateOf: () => "open",
    objectUrlOf: () => "blob:any",
    // Страница подменила геттер элемента, и он бросает: свидетель просмотра падает на чтении.
    viewingOf: (record) => {
      records.push(record);
      throw new TypeError("a page getter threw");
    },
    reportOddity: (what) => oddities.push(what),
  });
  const mime = 'video/mp4; codecs="avc1.42E01E"';
  const [older, newer] = [{ mediaSource: {}, sourceBuffer: {} }, { mediaSource: {}, sourceBuffer: {} }];
  tracker.sourceBufferAdded(older.mediaSource, older.sourceBuffer, mime);
  tracker.sourceBufferAdded(newer.mediaSource, newer.sourceBuffer, mime);
  const edges = [0, VIDEO_64_6S_MP4.initEnd, ...VIDEO_64_6S_MP4.mediaStarts.slice(1), file.length];
  // Оба MediaSource качают ролик целиком: документ переходит потолок внутри `appendBuffer` страницы.
  assert.doesNotThrow(() => {
    for (const source of [newer, older]) {
      for (let index = 0; index + 1 < edges.length; index += 1) tracker.bytesAppended(source.sourceBuffer, file.slice(edges[index], edges[index + 1]));
    }
  });

  assert.ok(budget.memory.totalBytes <= cap, "the cap is still enforced");
  assert.ok(records.length > 0, "the owner was asked");
  assert.ok(oddities.includes("could not read how a media source is being watched"), `oddities: ${JSON.stringify(oddities)}`);
  // О просмотре ничего не известно: память отдал самый старый MediaSource, младший цел.
  const replayOf = (mediaSource: object): Uint8Array => {
    const journal = tracker.recordOf(mediaSource)?.journal;
    assert.ok(journal !== undefined);
    return concat(journal.replay().flatMap((item) => (item.kind === "bytes" ? [item.view] : [])));
  };
  assert.deepEqual(replayOf(older.mediaSource), file.subarray(0, VIDEO_64_6S_MP4.initEnd));
  assert.deepEqual(replayOf(newer.mediaSource), file);
});

test("a layout read that throws while the budget picks who gives does not reach the page: that video counts as the farthest", () => {
  const file = VIDEO_64_6S_MP4.bytes();
  const startEnd = VIDEO_64_6S_MP4.mediaStarts[1];
  const oddities: string[] = [];
  // Влезают начала двух роликов из трёх: одному придётся отдать своё.
  const budget = new DocumentMemoryBudget(2 * startEnd + VIDEO_64_6S_MP4.initEnd + 500);
  const sources = [{ mediaSource: {}, sourceBuffer: {} }, { mediaSource: {}, sourceBuffer: {} }, { mediaSource: {}, sourceBuffer: {} }];
  const [near, broken, far] = sources;
  assert.ok(near !== undefined && broken !== undefined && far !== undefined);
  const tracker: MseTracker = new MseTracker({
    budget,
    readyStateOf: () => "open",
    objectUrlOf: () => "blob:any",
    viewingOf: (record) => ({
      kind: "not_started",
      place: {
        kind: "on_page",
        distanceFromView: () => {
          if (record === tracker.recordOf(broken.mediaSource)) throw new TypeError("a page getter threw");
          return record === tracker.recordOf(near.mediaSource) ? 50 : 4_000;
        },
      },
    }),
    reportOddity: (what) => oddities.push(what),
  });
  const mime = 'video/mp4; codecs="avc1.42E01E"';
  for (const source of sources) tracker.sourceBufferAdded(source.mediaSource, source.sourceBuffer, mime);
  assert.doesNotThrow(() => {
    for (const source of sources) {
      tracker.bytesAppended(source.sourceBuffer, file.slice(0, VIDEO_64_6S_MP4.initEnd));
      tracker.bytesAppended(source.sourceBuffer, file.slice(VIDEO_64_6S_MP4.initEnd, startEnd));
    }
  });

  assert.ok(oddities.includes("could not measure where a video stands on the page"), `oddities: ${JSON.stringify(oddities)}`);
  const replayOf = (mediaSource: object): Uint8Array => {
    const journal = tracker.recordOf(mediaSource)?.journal;
    assert.ok(journal !== undefined);
    return concat(journal.replay().flatMap((item) => (item.kind === "bytes" ? [item.view] : [])));
  };
  // Память отдало видео, место которого прочитать не вышло; остальные начала целы.
  assert.deepEqual(replayOf(broken.mediaSource), file.subarray(0, VIDEO_64_6S_MP4.initEnd));
  assert.deepEqual(replayOf(near.mediaSource), file.subarray(0, startEnd));
  assert.deepEqual(replayOf(far.mediaSource), file.subarray(0, startEnd));
});
