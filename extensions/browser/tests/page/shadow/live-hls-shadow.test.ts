// Тень эфира нативного HLS (сессия 10a3, решение владельца 232) на фейковом DOM: страница видит эфир
// (`duration = Infinity`, окно перемотки по `live_window`), время не прыгает при передаче (сдвиг P0 − J),
// перемотки переводятся между временем страницы и плеера и прижимаются к окну, возврат — на край эфира без T.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageEvent, ShadowCommand } from "../../../src/shared/handoff-contract.ts";
import type { ShadowSession } from "../../../src/page/shadow/index.ts";
import { LiveClockAlignment } from "../../../src/page/shadow/live/live-clock-alignment.ts";
import { flushMicrotasks, recordEvents, type FakeVideo } from "./fake-media-dom.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo, type ShadowHarness } from "./shadow-harness.ts";

const LIVE_URL = "https://live.example/channel/index.m3u8";
const SECOND_US = 1_000_000;
/** Секунда страницы при передаче (P0) и первый настоящий тик плеера (J): сдвиг = 7,5 − 45 = −37,5 с. */
const P0_S = 7.5;
const FIRST_REAL_TICK_S = 45;
const OFFSET_S = P0_S - FIRST_REAL_TICK_S;

interface PageTimeRanges {
  readonly length: number;
  start(index: number): number;
  end(index: number): number;
}

/** Чтение свойства так, как его читает страница (через прототипы с переопределениями). */
function pageRead(video: FakeVideo, name: string): unknown {
  return (video as unknown as Record<string, unknown>)[name];
}

function pageWrite(video: FakeVideo, name: string, value: unknown): void {
  (video as unknown as Record<string, unknown>)[name] = value;
}

/** Промежутки TimeRanges страницы как массив пар секунд. */
function spans(video: FakeVideo, name: "seekable" | "buffered"): [number, number][] {
  const ranges = pageRead(video, name) as PageTimeRanges;
  return Array.from({ length: ranges.length }, (_, index) => [ranges.start(index), ranges.end(index)]);
}

/** Играющий эфир Chromium на P0: длительность Infinity, свой `seekable` браузера пуст. */
function playingLive(harness: ShadowHarness): FakeVideo {
  const video = playingVideo(harness.dom, P0_S);
  video.real.src = LIVE_URL;
  video.real.duration = Number.POSITIVE_INFINITY;
  video.real.seekableRanges = [];
  return video;
}

/** Окно эфира во времени плеера, секунды. */
function liveWindow(edgeSeconds: number, startSeconds: number, endSeconds: number): ShadowCommand {
  return {
    kind: "live_window",
    edge_us: edgeSeconds * SECOND_US,
    seekable: { kind: "range", start_us: startSeconds * SECOND_US, end_us: endSeconds * SECOND_US },
  };
}

/** Тень эфира, плеер показал поколение (окно [41, 60,5], край 61) и прислал первый настоящий тик J. */
function alignedLive(): { harness: ShadowHarness; video: FakeVideo; session: ShadowSession } {
  const harness = createShadowHarness();
  const video = playingLive(harness);
  const session = harness.startLive(video);
  session.apply(liveWindow(61, 41, 60.5));
  harness.tick(session, FIRST_REAL_TICK_S, "playing");
  return { harness, video, session };
}

function eventsOfKind<Kind extends PageEvent["kind"]>(harness: ShadowHarness, kind: Kind): Extract<PageEvent, { kind: Kind }>[] {
  return emittedWithoutPulse(harness).filter((event): event is Extract<PageEvent, { kind: Kind }> => event.kind === kind);
}

describe("тень эфира: страница видит эфир", () => {
  it("duration = Infinity, источник снят, своё seekable браузера не показывается; окна нет — seekable и buffered пусты", async () => {
    const harness = createShadowHarness();
    const video = playingLive(harness);
    harness.startLive(video);
    await flushMicrotasks();
    assert.ok(video.real.srcObject instanceof harness.dom.classes.FakeMediaStream, "браузер эфир не качает");
    assert.equal(pageRead(video, "duration"), Number.POSITIVE_INFINITY);
    assert.equal(pageRead(video, "currentSrc"), LIVE_URL);
    assert.equal(pageRead(video, "networkState"), 2, "эфир всё время качается");
    assert.deepEqual(spans(video, "seekable"), []);
    assert.deepEqual(spans(video, "buffered"), []);
    assert.equal(pageRead(video, "currentTime"), P0_S);
  });

  it("seekable — окно плеера со сдвигом и едет за каждым новым окном; buffered — от позиции до края", () => {
    const { harness, video, session } = alignedLive();
    assert.ok(pageRead(video, "seekable") instanceof harness.dom.classes.FakeTimeRanges, "проверка бренда TimeRanges проходит");
    assert.deepEqual(spans(video, "seekable"), [[41 + OFFSET_S, 60.5 + OFFSET_S]]);
    assert.deepEqual(spans(video, "buffered"), [[P0_S, 61 + OFFSET_S]]);
    session.apply(liveWindow(63, 43, 62.5));
    assert.deepEqual(spans(video, "seekable"), [[43 + OFFSET_S, 62.5 + OFFSET_S]], "окно сдвинулось вместе с эфиром");
    assert.deepEqual(spans(video, "buffered"), [[P0_S, 63 + OFFSET_S]]);
  });

  it("эфир без DVR (seekable none): перематывать некуда, duration по-прежнему Infinity, buffered — до края", () => {
    const harness = createShadowHarness();
    const video = playingLive(harness);
    const session = harness.startLive(video);
    session.apply({ kind: "live_window", edge_us: 61 * SECOND_US, seekable: { kind: "none" } });
    harness.tick(session, FIRST_REAL_TICK_S, "playing");
    assert.deepEqual(spans(video, "seekable"), []);
    assert.deepEqual(spans(video, "buffered"), [[P0_S, 61 + OFFSET_S]]);
    pageWrite(video, "currentTime", 10);
    assert.deepEqual(eventsOfKind(harness, "page_seek"), [], "перемотка в эфире без окна плееру не уходит");
    assert.equal(pageRead(video, "currentTime"), P0_S);
    assert.equal(pageRead(video, "duration"), Number.POSITIVE_INFINITY);
  });
});

describe("тень эфира: время не прыгает (сопоставление часов)", () => {
  it("до показа поколения — P0 и ожидание, какие бы позиции ни несли тики; первый настоящий тик — ровно P0, дальше идёт", async () => {
    const harness = createShadowHarness();
    const video = playingLive(harness);
    const session = harness.startLive(video);
    const pageEvents = recordEvents(video, ["waiting", "playing", "seeking"]);
    harness.tick(session, 0, "waiting");
    harness.tick(session, 999, "playing");
    assert.equal(pageRead(video, "currentTime"), P0_S, "часы до показа — не настоящие");
    assert.equal(pageRead(video, "readyState"), 2, "страница ждёт данных");
    session.apply(liveWindow(61, 41, 60.5));
    harness.clock.advance(250);
    harness.tick(session, FIRST_REAL_TICK_S, "playing");
    assert.equal(pageRead(video, "currentTime"), P0_S, "в момент первого тика время не прыгнуло");
    harness.clock.advance(1_000);
    assert.equal(pageRead(video, "currentTime"), P0_S + 1, "интерполяция от P0");
    harness.tick(session, FIRST_REAL_TICK_S + 1, "playing");
    assert.equal(pageRead(video, "currentTime"), P0_S + 1, "тик J+1 — это P0+1");
    await flushMicrotasks();
    assert.ok(!pageEvents.types().includes("seeking"), "сопоставление — не перемотка");
    assert.ok(pageEvents.types().includes("playing"));
  });
});

describe("тень эфира: перемотки", () => {
  it("перемотка страницы внутри окна → page_seek во времени плеера (цель − сдвиг)", () => {
    const { harness, video } = alignedLive();
    pageWrite(video, "currentTime", 10);
    assert.deepEqual(eventsOfKind(harness, "page_seek"), [{ kind: "page_seek", target_us: (10 - OFFSET_S) * SECOND_US }]);
    assert.equal(pageRead(video, "currentTime"), 10);
    assert.equal(pageRead(video, "seeking"), true);
  });

  it("перемотка страницы за окно прижимается к краю окна (вперёд — к концу, назад — к началу)", () => {
    const { harness, video } = alignedLive();
    pageWrite(video, "currentTime", 100);
    assert.equal(pageRead(video, "currentTime"), 60.5 + OFFSET_S);
    pageWrite(video, "currentTime", 0);
    assert.equal(pageRead(video, "currentTime"), 41 + OFFSET_S);
    assert.deepEqual(
      eventsOfKind(harness, "page_seek").map((event) => event.target_us),
      [60.5 * SECOND_US, 41 * SECOND_US],
    );
  });

  it("перемотка страницы до первого настоящего тика (сдвиг неизвестен) — ничего: ни seeking, ни page_seek", async () => {
    const harness = createShadowHarness();
    const video = playingLive(harness);
    const session = harness.startLive(video);
    session.apply(liveWindow(61, 41, 60.5));
    const pageEvents = recordEvents(video, ["seeking", "seeked"]);
    pageWrite(video, "currentTime", 10);
    await flushMicrotasks();
    assert.deepEqual(eventsOfKind(harness, "page_seek"), []);
    assert.equal(pageRead(video, "currentTime"), P0_S);
    assert.equal(pageRead(video, "seeking"), false);
    assert.deepEqual(pageEvents.types(), []);
  });

  it("перемотка плеера: страница встаёт на цель + сдвиг, ответ player_seek_applied — во времени плеера", () => {
    const { harness, video, session } = alignedLive();
    session.apply({ kind: "seek", seek_id: 7, target_us: 50 * SECOND_US });
    assert.equal(pageRead(video, "currentTime"), 50 + OFFSET_S);
    assert.deepEqual(eventsOfKind(harness, "player_seek_applied"), [{ kind: "player_seek_applied", seek_id: 7, target_us: 50 * SECOND_US }]);
  });

  it("перемотка плеера раньше нуля страницы (отрицательный сдвиг): страница на 0, плееру — ровно его цель", () => {
    const { harness, video, session } = alignedLive();
    session.apply({ kind: "seek", seek_id: 9, target_us: 30 * SECOND_US });
    assert.equal(pageRead(video, "currentTime"), 0, "30 с плеера — до начала шкалы страницы");
    assert.deepEqual(eventsOfKind(harness, "player_seek_applied"), [{ kind: "player_seek_applied", seek_id: 9, target_us: 30 * SECOND_US }]);
  });

  it("окно, кончающееся ровно в нуле страницы, — законная точка [0, 0]; окно целиком раньше нуля — пусто", () => {
    const { video, session } = alignedLive();
    // Сдвиг −37,5 с: конец 37,5 с плеера — ровно 0 страницы.
    session.apply(liveWindow(61, 30, 37.5));
    assert.deepEqual(spans(video, "seekable"), [[0, 0]]);
    session.apply(liveWindow(61, 20, 30));
    assert.deepEqual(spans(video, "seekable"), []);
  });

  it("перемотка плеера до показа поколения: страница стоит на P0, плееру — его же цель", () => {
    const harness = createShadowHarness();
    const video = playingLive(harness);
    const session = harness.startLive(video);
    session.apply({ kind: "seek", seek_id: 3, target_us: 50 * SECOND_US });
    assert.equal(pageRead(video, "currentTime"), P0_S);
    assert.deepEqual(eventsOfKind(harness, "player_seek_applied"), [{ kind: "player_seek_applied", seek_id: 3, target_us: 50 * SECOND_US }]);
  });

  it("конец эфира (ended плеера) — как у остальных видов: pause и ended странице", async () => {
    const { harness, video, session } = alignedLive();
    const pageEvents = recordEvents(video, ["pause", "ended"]);
    harness.tick(session, FIRST_REAL_TICK_S + 2, "ended");
    await flushMicrotasks();
    assert.equal(pageRead(video, "ended"), true);
    assert.deepEqual(pageEvents.types(), ["pause", "ended"]);
  });
});

describe("тень эфира: возврат на страницу — на край эфира, не на T", () => {
  /** Браузер заново загрузил эфир и сам вошёл в него (Chromium 156: начало текущего окна, своя шкала от нуля). */
  function browserJoinsLive(video: FakeVideo, atSeconds: number): void {
    video.real.readyState = 4;
    video.real.currentTime = atSeconds;
    video.fireTrusted("loadedmetadata");
  }

  const currentTimeWrites = (video: FakeVideo) => video.realCalls.filter((call) => call.kind === "set" && call.property === "currentTime");

  // Решение 234: эфир возвращается с игрой при любом «resume» плеера, и `play()` идёт вместе с возвратом
  // источника — ДО `loadedmetadata`. Загруженный на паузе эфир встроенный HLS Chromium 156 потом не запускает.
  for (const resume of ["playing", "paused"] as const) {
    it(`возврат с «${resume}»: src перезагружен, T не записано, play() вместе с перезагрузкой, элемент играет`, async () => {
      const { harness, video, session } = alignedLive();
      await flushMicrotasks();
      video.realCalls.length = 0;
      session.end({ kind: "at", position_us: 50 * SECOND_US }, { kind: "exactly", resume });
      assert.deepEqual(emittedWithoutPulse(harness).at(-1), { kind: "handoff_ended", cause: "returned" });
      assert.equal(video.real.srcObject, null, "эфир возвращён элементу");
      const playsWithReload = video.realCalls.filter((call) => call.kind === "play").length;
      assert.equal(playsWithReload, 1, "play() — в той же задаче, что и возврат источника, до метаданных");
      assert.equal(video.real.paused, false);
      browserJoinsLive(video, 0.2);
      await flushMicrotasks();
      await flushMicrotasks();
      assert.deepEqual(currentTimeWrites(video), [], "ни T, ни своих перемоток");
      assert.equal(pageRead(video, "currentTime"), 0.2, "тень снята — страница видит настоящий элемент у края эфира");
      assert.equal(pageRead(video, "duration"), Number.POSITIVE_INFINITY);
      assert.equal(pageRead(video, "paused"), false, "эфир играет, на паузу его никто не поставил");
      // Единственный pause() — сброс автозапуска при записи источника (`#writeOwnSource`), он ДО play().
      const firstPlay = video.realCalls.findIndex((call) => call.kind === "play");
      assert.equal(
        video.realCalls.slice(firstPlay).some((call) => call.kind === "pause"),
        false,
        "после play() ни одного pause() настоящему элементу",
      );
      assert.equal(harness.timers.pending(), 0, "страховочный таймер снят");
    });
  }

  it("страница под тенью видела паузу: после возврата эфира она получает play (настоящий погасил шлагбаум)", async () => {
    const { video, session } = alignedLive();
    // Зритель поставил паузу в плеере: страница под тенью видит паузу.
    session.apply({ kind: "pause" });
    await flushMicrotasks();
    assert.equal(pageRead(video, "paused"), true, "под тенью страница видит паузу");
    const pageEvents = recordEvents(video, ["play", "pause"]);
    session.end({ kind: "at", position_us: 50 * SECOND_US }, { kind: "exactly", resume: "paused" });
    browserJoinsLive(video, 0.2);
    await flushMicrotasks();
    await flushMicrotasks();
    assert.equal(pageRead(video, "paused"), false);
    assert.deepEqual(pageEvents.types().filter((type) => type === "play"), ["play"], "ровно одно play странице");
    assert.equal(pageEvents.types().includes("pause"), false);
  });
});

describe("регрессия: тень файла и записи HLS окна эфира не знают", () => {
  it("live_window у тени файла не меняет seekable и перемотку; page_seek — цель как есть", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    video.real.src = "https://cdn.example/vod/master.m3u8";
    const session = harness.startFile(video);
    session.apply(liveWindow(61, 41, 60.5));
    harness.tick(session, 10, "playing");
    assert.deepEqual(spans(video, "seekable"), [[0, 100]]);
    assert.equal(pageRead(video, "duration"), 100);
    pageWrite(video, "currentTime", 80);
    assert.deepEqual(eventsOfKind(harness, "page_seek"), [{ kind: "page_seek", target_us: 80 * SECOND_US }]);
    assert.equal(pageRead(video, "currentTime"), 80);
  });

  it("ответ на перемотку плеера у тени файла — как до 10a3: цель, прижатая к длительности", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    video.real.src = "https://cdn.example/media/movie.mp4";
    const session = harness.startFile(video);
    session.apply({ kind: "seek", seek_id: 4, target_us: 500 * SECOND_US });
    assert.deepEqual(eventsOfKind(harness, "player_seek_applied"), [{ kind: "player_seek_applied", seek_id: 4, target_us: 100 * SECOND_US }]);
  });
});

describe("сопоставление часов эфира: LiveClockAlignment", () => {
  it("отрицательный сдвиг: моменты эфира раньше нуля страницы прижимаются к нулю в обе стороны", () => {
    const alignment = new LiveClockAlignment(2 * SECOND_US);
    assert.equal(alignment.isAligned, false);
    assert.deepEqual(alignment.pageTimeOf(30 * SECOND_US), { kind: "offset_unknown" });
    alignment.notePlayerShowsGeneration();
    // Пометка «чьи часы» (16a2) проходит перевод времени как есть.
    assert.deepEqual(alignment.pageTickOf({ position_us: 30 * SECOND_US, state: "playing", rate: 1000, relation: "this_video" }), {
      position_us: 2 * SECOND_US,
      state: "playing",
      rate: 1000,
      relation: "this_video",
    });
    assert.equal(alignment.isAligned, true);
    assert.deepEqual(alignment.pageTimeOf(10 * SECOND_US), { kind: "mapped", us: 0 });
    assert.deepEqual(alignment.pageTimeOf(31 * SECOND_US), { kind: "mapped", us: 3 * SECOND_US });
    assert.deepEqual(alignment.playerTimeOf(3 * SECOND_US), { kind: "mapped", us: 31 * SECOND_US });
  });

  it("сдвиг фиксируется один раз: следующие окна и тики его не меняют", () => {
    const alignment = new LiveClockAlignment(100 * SECOND_US);
    alignment.notePlayerShowsGeneration();
    alignment.pageTickOf({ position_us: 20 * SECOND_US, state: "waiting", rate: 1000, relation: "unknown" });
    alignment.notePlayerShowsGeneration();
    assert.deepEqual(alignment.pageTickOf({ position_us: 25 * SECOND_US, state: "playing", rate: 1500, relation: "previous_video" }), {
      position_us: 105 * SECOND_US,
      state: "playing",
      rate: 1500,
      relation: "previous_video",
    });
    assert.deepEqual(alignment.playerTimeOf(50 * SECOND_US), { kind: "mapped", us: 0 }, "цель раньше начала плеера — его ноль");
  });
});
