// Тень эфира нативного HLS, когда эфир кончился (протокол 1.11 `recording_window`, сессия 16d1, решения 325–327),
// на фейковом DOM: страница видит конечную длительность и одно `durationchange`, как у настоящего элемента при
// `ENDLIST` (зонд Chromium 156); перемотка — по окну записи; возврат — на ту же секунду, считая от конца.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageEvent, ShadowCommand } from "../../../src/shared/handoff-contract.ts";
import type { ShadowSession } from "../../../src/page/shadow/index.ts";
import { flushMicrotasks, recordEvents, type FakeVideo } from "./fake-media-dom.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo, type ShadowHarness } from "./shadow-harness.ts";

const LIVE_URL = "https://live.example/channel/index.m3u8";
const SECOND_US = 1_000_000;
/** Секунда страницы при передаче (P0) и первый настоящий тик плеера (J): сдвиг = 7,5 − 45 = −37,5 с. */
const P0_S = 7.5;
const FIRST_REAL_TICK_S = 45;
const OFFSET_S = P0_S - FIRST_REAL_TICK_S;
/** Край записи во времени плеера: эфир кончился на 70 с. */
const RECORDING_END_S = 70;

interface PageTimeRanges {
  readonly length: number;
  start(index: number): number;
  end(index: number): number;
}

function pageRead(video: FakeVideo, name: string): unknown {
  return (video as unknown as Record<string, unknown>)[name];
}

function pageWrite(video: FakeVideo, name: string, value: unknown): void {
  (video as unknown as Record<string, unknown>)[name] = value;
}

function spans(video: FakeVideo, name: "seekable" | "buffered"): [number, number][] {
  const ranges = pageRead(video, name) as PageTimeRanges;
  return Array.from({ length: ranges.length }, (_, index) => [ranges.start(index), ranges.end(index)]);
}

function playingLive(harness: ShadowHarness): FakeVideo {
  const video = playingVideo(harness.dom, P0_S);
  video.real.src = LIVE_URL;
  video.real.duration = Number.POSITIVE_INFINITY;
  video.real.seekableRanges = [];
  return video;
}

function liveWindow(edgeSeconds: number, startSeconds: number, endSeconds: number): ShadowCommand {
  return {
    kind: "live_window",
    edge_us: edgeSeconds * SECOND_US,
    seekable: { kind: "range", start_us: startSeconds * SECOND_US, end_us: endSeconds * SECOND_US },
  };
}

/** Окно записи во времени плеера: край записи и окно перемотки, секунды. */
function recordingWindow(endSeconds: number, startSeconds: number, seekableEndSeconds: number): ShadowCommand {
  return {
    kind: "recording_window",
    end_us: endSeconds * SECOND_US,
    seekable: { kind: "range", start_us: startSeconds * SECOND_US, end_us: seekableEndSeconds * SECOND_US },
  };
}

/** Тень эфира с известным сдвигом: плеер показал поколение и прислал первый настоящий тик. */
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

describe("эфир кончился: страница видит запись", () => {
  it("duration — край записи со сдвигом, одно durationchange, seekable — окно записи, качать больше нечего", async () => {
    const { video, session } = alignedLive();
    const pageEvents = recordEvents(video, ["durationchange"]);
    session.apply(recordingWindow(RECORDING_END_S, 41, 66));
    await flushMicrotasks();

    assert.equal(pageRead(video, "duration"), RECORDING_END_S + OFFSET_S);
    assert.deepEqual(pageEvents.types(), ["durationchange"]);
    assert.deepEqual(spans(video, "seekable"), [[41 + OFFSET_S, 66 + OFFSET_S]]);
    assert.deepEqual(spans(video, "buffered"), [[P0_S, RECORDING_END_S + OFFSET_S]]);
    assert.equal(pageRead(video, "networkState"), 1, "NETWORK_IDLE: запись больше не качается");
  });

  it("окно перемотки дорастает до края, а длительность та же — новых durationchange нет", async () => {
    const { video, session } = alignedLive();
    const pageEvents = recordEvents(video, ["durationchange"]);
    session.apply(recordingWindow(RECORDING_END_S, 41, 66));
    session.apply(recordingWindow(RECORDING_END_S, 41, 68));
    session.apply(recordingWindow(RECORDING_END_S, 41, RECORDING_END_S));
    await flushMicrotasks();

    assert.deepEqual(pageEvents.types(), ["durationchange"], "длительность объявлена один раз");
    assert.deepEqual(spans(video, "seekable"), [[41 + OFFSET_S, RECORDING_END_S + OFFSET_S]]);
  });

  it("запоздавшее окно эфира запись не отменяет: эфир решается при открытии", () => {
    const { video, session } = alignedLive();
    session.apply(recordingWindow(RECORDING_END_S, 41, 66));
    session.apply(liveWindow(80, 50, 79));

    assert.equal(pageRead(video, "duration"), RECORDING_END_S + OFFSET_S);
    assert.deepEqual(spans(video, "seekable"), [[41 + OFFSET_S, 66 + OFFSET_S]]);
  });

  it("эфир кончился ровно к открытию плеером: окно записи — первое, длительность — с первым настоящим тиком", async () => {
    const harness = createShadowHarness();
    const video = playingLive(harness);
    const session = harness.startLive(video);
    const pageEvents = recordEvents(video, ["durationchange"]);
    session.apply(recordingWindow(RECORDING_END_S, 0, RECORDING_END_S));
    assert.equal(pageRead(video, "duration"), Number.POSITIVE_INFINITY, "сдвиг ещё неизвестен — время страницы не назвать");

    harness.tick(session, FIRST_REAL_TICK_S, "playing");
    await flushMicrotasks();

    assert.equal(pageRead(video, "currentTime"), P0_S, "время не прыгнуло");
    assert.equal(pageRead(video, "duration"), RECORDING_END_S + OFFSET_S);
    assert.deepEqual(pageEvents.types(), ["durationchange"]);
  });

  it("перемотка страницы внутри записи — page_seek во времени плеера; за окном — прижата к его краю", () => {
    const { harness, video, session } = alignedLive();
    session.apply(recordingWindow(RECORDING_END_S, 41, 66));

    pageWrite(video, "currentTime", 10);
    pageWrite(video, "currentTime", 1_000);

    const targets = eventsOfKind(harness, "page_seek").map((event) => event.target_us);
    assert.deepEqual(targets, [(10 - OFFSET_S) * SECOND_US, 66 * SECOND_US]);
  });
});

describe("законченный эфир возвращается на ту же секунду, считая от конца (решение 326)", () => {
  /** Браузер заново загрузил законченный плейлист: запись длиной в последнее окно сайта, отсчёт от нуля. */
  function browserLoadsTheEndedPlaylist(video: FakeVideo, durationSeconds: number): void {
    video.real.readyState = 4;
    video.real.currentTime = 0;
    video.real.duration = durationSeconds;
    video.fireTrusted("loadedmetadata");
  }

  const currentTimeWrites = (video: FakeVideo) =>
    video.realCalls.filter((call) => call.kind === "set" && call.property === "currentTime").map((call) => (call.kind === "set" ? call.value : undefined));

  it("пауза — на паузе: элемент встаёт за 4 с до конца нового окна, play() вместе с перезагрузкой не зовётся", async () => {
    const { harness, video, session } = alignedLive();
    session.apply(recordingWindow(RECORDING_END_S, 41, RECORDING_END_S));
    session.apply({ kind: "pause" });
    await flushMicrotasks();
    video.realCalls.length = 0;

    // Плеер стоял на 66 с записи длиной 70 с: до конца 4 с.
    session.end({ kind: "at", position_us: 66 * SECOND_US }, { kind: "exactly", resume: "paused" });
    assert.equal(video.realCalls.filter((call) => call.kind === "play").length, 0, "запись, не эфир: исключение 234 не нужно");
    browserLoadsTheEndedPlaylist(video, 8);
    await flushMicrotasks();
    await flushMicrotasks();

    assert.deepEqual(currentTimeWrites(video), [4], "8 − (70 − 66) = 4 с в шкале элемента");
    assert.equal(pageRead(video, "paused"), true);
    assert.deepEqual(emittedWithoutPulse(harness).filter((event) => event.kind === "handoff_ended"), [{ kind: "handoff_ended", cause: "returned" }]);
  });

  it("игра — с игрой", async () => {
    const { video, session } = alignedLive();
    session.apply(recordingWindow(RECORDING_END_S, 41, RECORDING_END_S));
    await flushMicrotasks();
    video.realCalls.length = 0;

    session.end({ kind: "at", position_us: 66 * SECOND_US }, { kind: "exactly", resume: "playing" });
    browserLoadsTheEndedPlaylist(video, 8);
    await flushMicrotasks();
    await flushMicrotasks();

    assert.deepEqual(currentTimeWrites(video), [4]);
    assert.equal(pageRead(video, "paused"), false);
  });

  it("секунда выпала из окна сайта — начало окна, не раньше нуля", async () => {
    const { video, session } = alignedLive();
    session.apply(recordingWindow(RECORDING_END_S, 41, RECORDING_END_S));
    await flushMicrotasks();
    video.realCalls.length = 0;

    // До конца 20 с, а окно сайта — 8 с.
    session.end({ kind: "at", position_us: 50 * SECOND_US }, { kind: "exactly", resume: "paused" });
    browserLoadsTheEndedPlaylist(video, 8);
    await flushMicrotasks();
    await flushMicrotasks();

    // Элемент и так на нуле: перематывать его туда незачем.
    assert.deepEqual(currentTimeWrites(video), []);
    assert.equal(pageRead(video, "currentTime"), 0);
  });
});

describe("регрессия: тень файла окна записи не знает", () => {
  it("recording_window у тени файла не меняет длительность и перемотку", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    video.real.src = "https://cdn.example/vod/master.m3u8";
    const session = harness.startFile(video);
    session.apply(recordingWindow(RECORDING_END_S, 41, 66));
    harness.tick(session, 10, "playing");

    assert.equal(pageRead(video, "duration"), 100);
    assert.deepEqual(spans(video, "seekable"), [[0, 100]]);
  });
});
