// Пометка часов плеера через весь фон (сессия 16a2, протокол 1.10): тень получает тик с отношением к текущему
// видео, а часы прошлого видео не открывают переезд снова. Мир — как у переезда с рекламы на фильм (09c2).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { PROTOCOL_VERSION_1_9, type ClockState, type PageSessionMessage, type ProtocolVersion } from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type ClockRelation, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { MP4, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { grantFor } from "./support.ts";

const AREA = 296_000;
/** Реклама: файл с неизвестного домена, рядом ждёт фильм (форма Ashdi и Tortuga). */
const AD: VideoCandidate = {
  ...DEFAULT_CANDIDATE_FACTS,
  document: "doc-ad",
  element: 3,
  media: { kind: "file" },
  file_host: "video.unknown-ad-cdn.example",
  playing: true,
  visible_area_px: AREA,
  duration_s: 24,
};
const FILM: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: AREA, duration_s: 6381 };
const HLS_FILM: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-hls", element: 4, media: { kind: "hls" }, playing: true, visible_area_px: AREA, file_host: "cdn.example" };

const heldVideo = (element: number, candidate: VideoCandidate): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element, candidates: [{ ...candidate, playing: true }], trigger: "next_video" });

function adWorld(agreed?: ProtocolVersion): World {
  const world = setup([{ frameId: 0, result: [FILM, AD] }]);
  if (agreed !== undefined) world.door.nextOutcome = (request) => grantFor(request, "started", agreed);
  return world;
}

/** Номер последнего поколения, которое фон объявил плееру (`generation_started`). */
function lastAnnouncedGeneration(world: World): number {
  const announced = world.sockets
    .last()
    .sentMessages()
    .filter((message): message is Extract<PageSessionMessage, { type: "generation_started" }> => message.type === "generation_started")
    .at(-1);
  assert.ok(announced !== undefined, "поколение не объявлено");
  return announced.generation;
}

/** Плеер прислал тик часов; `generation` — номер поколения тика (протокол 1.10), `undefined` — без номера. */
function playerClock(world: World, state: ClockState, generation: number | undefined, positionUs = 12_000_000): void {
  const tick = { type: "clock", seq: 1, position_us: positionUs, state, rate: 1000 } as const;
  world.sockets.last().receive(generation === undefined ? tick : { ...tick, generation });
}

/** Пометка последнего тика, ушедшего тени. */
function lastShadowClockRelation(world: World): ClockRelation | undefined {
  const posted = world.framePorts.last().posted.filter((command): command is Extract<PageCommand, { kind: "shadow" }> => command.kind === "shadow");
  const clock = posted.map((command) => command.command).filter((command) => command.kind === "clock").at(-1);
  return clock?.relation;
}

function lastBeginRequest(world: World): number {
  const begin = world.framePorts.last().posted.filter((command) => command.kind === "begin_handoff").at(-1);
  assert.ok(begin?.kind === "begin_handoff", "begin_handoff не отправлен");
  return begin.request_id;
}

/** Реклама-файл в плеере (поколение 1), её пропустили посреди ролика, страница запустила фильм и тень встала на нём. */
async function skipAdToFilm(world: World): Promise<{ readonly adGeneration: number }> {
  await startHandoff(world);
  const port = world.framePorts.last();
  port.deliver({ kind: "file_source", url: "https://video.unknown-ad-cdn.example/ad.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
  const adGeneration = lastAnnouncedGeneration(world);
  playerClock(world, "playing", adGeneration, 5_000_000);
  // Пропуск посреди рекламы: её `ended` до фона не дошёл, запасное правило 1.9 (`staleClockExpected`) не взведено.
  port.deliver({ kind: "handoff_ended", cause: "element_released" });
  assert.equal(world.handoffs.heldVideo(TAB, 0, heldVideo(FILM.element, FILM)), "adopt");
  port.deliver({ kind: "handoff_began", request_id: lastBeginRequest(world), outcome: "started" });
  return { adGeneration };
}

describe("пометка часов для тени (16a2)", () => {
  it("тик объявленного поколения текущего видео уходит тени как this_video, тик без номера — как unknown", async () => {
    const world = adWorld();
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: "https://video.unknown-ad-cdn.example/ad.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    playerClock(world, "playing", lastAnnouncedGeneration(world));
    assert.equal(lastShadowClockRelation(world), "this_video");
    playerClock(world, "playing", undefined);
    assert.equal(lastShadowClockRelation(world), "unknown");
  });

  it("окно переезда: ended старого номера сразу после handoff_began не открывает переезд и уходит тени как previous_video", async () => {
    const world = adWorld();
    const { adGeneration } = await skipAdToFilm(world);
    // Плеер ещё показывает рекламу: её конец пришёл, когда тень уже на фильме, а поколение фильма не объявлено.
    playerClock(world, "ended", adGeneration);
    assert.equal(lastShadowClockRelation(world), "previous_video", "часы рекламы не выдаются тени фильма за её собственные");
    const anotherVideo: VideoCandidate = { ...FILM, element: 8 };
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldVideo(anotherVideo.element, anotherVideo)), "release", "фильм идёт — переезд закрыт");
    assert.deepEqual(world.ended, []);
  });

  it("после объявления поколения фильма его тики — this_video, а запоздалые тики рекламы — previous_video", async () => {
    const world = adWorld();
    const { adGeneration } = await skipAdToFilm(world);
    world.framePorts.last().deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
    const filmGeneration = lastAnnouncedGeneration(world);
    assert.ok(filmGeneration > adGeneration);
    playerClock(world, "playing", filmGeneration, 11_100_000);
    assert.equal(lastShadowClockRelation(world), "this_video");
    playerClock(world, "ended", adGeneration);
    assert.equal(lastShadowClockRelation(world), "previous_video");
    // Настоящий конец фильма (его номер) переезд открывает, как раньше.
    playerClock(world, "ended", filmGeneration, 6_381_000_000);
    const anotherVideo: VideoCandidate = { ...FILM, element: 8 };
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldVideo(anotherVideo.element, anotherVideo)), "adopt");
  });

  it("реклама доиграла (ended своего номера), фильм с номером сразу кончился: конец фильма засчитан, запасное правило 1.9 его не глотает", async () => {
    const world = adWorld();
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: "https://video.unknown-ad-cdn.example/ad.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    const adGeneration = lastAnnouncedGeneration(world);
    // Реклама доиграла у плеера: переезд открыт, после него взведено запасное правило «чужой ended».
    playerClock(world, "ended", adGeneration, 24_000_000);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldVideo(FILM.element, FILM)), "adopt");
    port.deliver({ kind: "handoff_began", request_id: lastBeginRequest(world), outcome: "started" });
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
    const filmGeneration = lastAnnouncedGeneration(world);
    // Первый же тик фильма — его конец (номер фильма): это правда о текущем видео, переезд снова открыт.
    playerClock(world, "ended", filmGeneration, 6_381_000_000);
    const anotherVideo: VideoCandidate = { ...FILM, element: 8 };
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldVideo(anotherVideo.element, anotherVideo)), "adopt");
  });

  it("сторона 1.9: тот же сценарий без номеров — прежние правила (пометка unknown)", async () => {
    const world = adWorld(PROTOCOL_VERSION_1_9);
    await skipAdToFilm(world);
    playerClock(world, "ended", undefined);
    assert.equal(lastShadowClockRelation(world), "unknown");
  });

  it("HLS: номер тика равен номеру поколения hls_source_added — this_video", async () => {
    const world = setup([{ frameId: 0, result: [HLS_FILM] }]);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "hls_source", url: "https://cdn.example/vod/master.m3u8", referrer_origin: null, referrer_policy: "no-referrer", stream: "vod" });
    const added = world.sockets
      .last()
      .sentMessages()
      .find((message): message is Extract<PageSessionMessage, { type: "hls_source_added" }> => message.type === "hls_source_added");
    assert.ok(added !== undefined, "HLS не объявлен");
    assert.equal(added.generation, lastAnnouncedGeneration(world));
    playerClock(world, "playing", added.generation);
    assert.equal(lastShadowClockRelation(world), "this_video");
  });
});
