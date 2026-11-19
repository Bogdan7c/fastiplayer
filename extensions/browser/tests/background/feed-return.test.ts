// «Назад» в ленту после ухода из неё (сессия 16b3b, решения 375–376) на фоне целиком.
//
// Уход из ленты в другой раздел кончает сеанс: окно на паузе, «Source tab closed» (370). В срок «Page reload»
// (по умолчанию 30 с) фон помнит раздел ленты: вкладка вернулась туда «Назад», страница снова играет ролик — он
// уходит в окно новым сеансом (процесс окна ещё жив, плеер ставит новый сеанс в то же окно). Проверяется провод
// плееру (второй `open_session`, `begin_handoff` нового сеанса), ответы сторожу и что окно не уводят чужие ролики.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FEED_SECTION_LEARN_MS } from "../../src/background/handoff/feed/session-feed.ts";
import { DEFAULT_PAGE_RELOAD_WAIT_MS } from "../../src/shared/page-reload-wait-settings.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { flushPromises } from "./support.ts";
import { MP4, setup, startHandoff, TAB, type World, type WorldOptions } from "./handoff-world.ts";

const REELS = "https://www.instagram.com/reels/C7xYz1AbCdE/";
const NEXT_REEL = "https://www.instagram.com/reels/DBq-9_kLmNo/";
const EXPLORE = "https://www.instagram.com/explore/";

const REEL_AREA = 164_157;

function reel(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return {
    ...DEFAULT_CANDIDATE_FACTS,
    document: "doc-feed",
    element,
    media: { kind: "media_source", in_worker: false },
    playing: true,
    visible_area_px: REEL_AREA,
    duration_s: 10 + element,
    ...overrides,
  };
}

/** Тихое превью «Интересного»: виден, играет, без звука. */
const SILENT_PREVIEW: VideoCandidate = reel(50, { media: { kind: "file" }, file_host: "www.instagram.com", muted: true, duration_s: 8 });

const report = (candidate: VideoCandidate): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger: "next_video", start_data: "available" });

type BeginCommand = Extract<PageCommand, { kind: "begin_handoff" }>;

function lastBeginFor(world: World, element: number): BeginCommand {
  const begin = world.framePorts.last().posted.findLast((command): command is BeginCommand => command.kind === "begin_handoff" && command.element === element);
  assert.ok(begin !== undefined, `begin_handoff для ролика ${element} не отправлен`);
  return begin;
}

function streamOf(world: World, element: number): void {
  const port = world.framePorts.last();
  port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: `ms-${element}`, source_buffer: 0, mime_type: MP4 } });
  port.deliver({ kind: "bytes", media_source: `ms-${element}`, source_buffer: 0, bytes: new Uint8Array([element]) });
}

function liveFor(world: World, ms: number): void {
  for (let elapsed = 0; elapsed < ms; elapsed += 1_000) {
    world.framePorts.last().deliver({ kind: "shadow_alive" });
    world.clock.advance(Math.min(1_000, ms - elapsed));
  }
}

/** Лента: ролик 1 передан, шаг на ролик 2 (раздел ленты выучен), затем уход в «Интересное» (I6). */
async function sessionThatLeftTheFeed(options: WorldOptions = {}): Promise<World> {
  const world = setup([{ frameId: 0, result: [reel(1), reel(2, { playing: false, visible_area_px: 0 })] }], undefined, undefined, options);
  world.navigateTab(REELS);
  await startHandoff(world);
  streamOf(world, 1);
  const port = world.framePorts.last();
  port.deliver({ kind: "page_playback", request: { kind: "pause" } });
  port.deliver({ kind: "video_left_screen", exit: "scrolled_off_screen" });
  assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "adopt");
  world.updateTab({ url: NEXT_REEL });
  port.deliver({ kind: "handoff_began", request_id: lastBeginFor(world, 2).request_id, outcome: "started" });
  streamOf(world, 2);
  liveFor(world, FEED_SECTION_LEARN_MS + 1);
  // Уход: адрес «Интересного», ролик вынут и на паузе, тень снята страницей.
  world.updateTab({ url: EXPLORE });
  world.clock.advance(500);
  port.deliver({ kind: "page_playback", request: { kind: "pause" } });
  port.deliver({ kind: "video_left_screen", exit: "removed_from_document" });
  world.clock.advance(1_000);
  port.deliver({ kind: "handoff_ended", cause: "element_released" });
  assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }], "уход из ленты — честный конец (16b3)");
  await flushPromises();
  return world;
}

/** Довести новый сеанс подхвата до «started»: плеер принял, страница начала тень на ролике `element`. */
async function pickedUpSessionStands(world: World, element: number): Promise<void> {
  await flushPromises();
  const socket = world.sockets.last();
  socket.open();
  socket.receive({ type: "attached" });
  await flushPromises();
  world.framePorts.last().deliver({ kind: "handoff_began", request_id: lastBeginFor(world, element).request_id, outcome: "started" });
  await flushPromises();
}

async function videoInPlayer(world: World): Promise<number | null> {
  const survey = await world.handoffs.surveyTab(TAB);
  assert.equal(survey.kind, "surveyed");
  const presence = survey.kind === "surveyed" ? survey.in_player : { kind: "no_session" as const };
  return presence.kind === "in_player" ? (presence.video?.element ?? null) : null;
}

describe("«Назад» в ленту: ролик уходит в то же окно (решение 375)", () => {
  it("вкладка вернулась в раздел ленты, страница играет прежний ролик — новый сеанс с этим роликом", async () => {
    const world = await sessionThatLeftTheFeed();
    const armingsBefore = world.armings.length;
    world.updateTab({ url: REELS });
    assert.ok(world.armings.length > armingsBefore, "в разделе ленты сторож взведён");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "adopt_new_session");
    await pickedUpSessionStands(world, 1);
    assert.equal(world.door.requests.length, 2, "плееру — второй open_session: тот же процесс окна ставит новый сеанс");
    assert.equal(await videoInPlayer(world), 1);
    assert.deepEqual(world.releases, [], "видео взято, отпускать нечего");
  });

  it("ушли до первого шага ленты (раздел не выучен) — раздел ленты тот, где встал ролик; «Назад» туда подхватывает", async () => {
    const world = setup([{ frameId: 0, result: [reel(1)] }]);
    world.navigateTab(REELS);
    await startHandoff(world);
    streamOf(world, 1);
    world.updateTab({ url: EXPLORE });
    const port = world.framePorts.last();
    port.deliver({ kind: "page_playback", request: { kind: "pause" } });
    port.deliver({ kind: "video_left_screen", exit: "removed_from_document" });
    port.deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    await flushPromises();
    world.updateTab({ url: NEXT_REEL });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "adopt_new_session", "/reels/<другой id>/ — тот же раздел ленты");
  });

  it("продление взвода идёт, пока вкладка в ленте, и стоит, пока она снова в другом разделе", async () => {
    const world = await sessionThatLeftTheFeed();
    const armingsAfterLeave = world.armings.length;
    world.clock.advance(5_000);
    assert.equal(world.armings.length, armingsAfterLeave, "в «Интересном» сторож не взводится: там своё превью");
    world.updateTab({ url: REELS });
    world.clock.advance(3_000);
    const armedInFeed = world.armings.length - armingsAfterLeave;
    assert.ok(armedInFeed >= 3, `в ленте взвод продлевается: ${armedInFeed}`);
    world.updateTab({ url: EXPLORE });
    const armingsAfterSecondLeave = world.armings.length;
    world.clock.advance(5_000);
    assert.equal(world.armings.length, armingsAfterSecondLeave);
  });
});

describe("«Назад» в ленту: чего подхват не берёт", () => {
  it("тихое превью «Интересного», пока вкладка в другом разделе, — отпустить, нового сеанса нет", async () => {
    const world = await sessionThatLeftTheFeed();
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(SILENT_PREVIEW)), "release");
    await flushPromises();
    assert.equal(world.door.requests.length, 1);
  });

  it("срок «Page reload» вышел — запись забыта, ролик ленты играет в браузере", async () => {
    const world = await sessionThatLeftTheFeed();
    world.clock.advance(DEFAULT_PAGE_RELOAD_WAIT_MS + 1);
    world.updateTab({ url: REELS });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "release");
    await flushPromises();
    assert.equal(world.door.requests.length, 1);
  });

  it("ожидание перезагрузки выключено (0) — записи нет вовсе", async () => {
    const world = await sessionThatLeftTheFeed({ pageReloadWait: { seconds: 0 } });
    world.updateTab({ url: REELS });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "release");
  });

  it("вкладку закрыли — запись забыта", async () => {
    const world = await sessionThatLeftTheFeed();
    world.handoffs.tabRemoved(TAB);
    world.updateTab({ url: REELS });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "release");
  });

  it("сеанс кончился не уходом из ленты (страница забрала видео, 4009) — подхвата нет", async () => {
    const world = setup([{ frameId: 0, result: [reel(1)] }]);
    world.navigateTab(REELS);
    await startHandoff(world);
    streamOf(world, 1);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
    await flushPromises();
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "release");
  });

  it("передача вернувшегося ролика не состоялась — видео отпущено во фрейме, второй ролик ждёт свою очередь", async () => {
    const world = await sessionThatLeftTheFeed();
    world.updateTab({ url: REELS });
    // Опрос вкладки не находит видео: передача кончается отказом.
    world.setFrames([{ frameId: 0, result: [] }]);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "adopt_new_session");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "release", "пока идёт передача, второе видео не берётся");
    await flushPromises();
    await flushPromises();
    assert.deepEqual(world.releases, [{ tabId: TAB, frameId: 0, element: 1 }]);
  });

  it("новый сеанс вкладки (кнопка) — запись забыта: следующий уход из ленты решит своё", async () => {
    const world = await sessionThatLeftTheFeed();
    world.updateTab({ url: REELS });
    world.setFrames([{ frameId: 0, result: [reel(3)] }]);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    await flushPromises();
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(1))), "release", "старой записи больше нет");
  });
});
