// Окно идёт за лентой (16b2, решения 354–357, 359): переданный ролик ушёл с экрана и страница его
// остановила — сеанс переезжает на ролик, который страница запустила следующим, в том же окне. Формы — как
// у живых лент: Instagram (много элементов, пауза ушедшего, следующий без звука), TikTok (ушедший вынут из
// документа). Проверяется то, что видят плеер (провод, одно окно, поколения) и страница (команды тени).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { NEXT_EPISODE_WAIT_MS } from "../../src/shared/next-episode-wait.ts";
import { NEXT_VIDEO_WAIT_MS } from "../../src/background/handoff/video-relocation.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { setup, startHandoff, TAB, type World } from "./handoff-world.ts";

/** Площадь ролика ленты на экране (столбец ленты Instagram в окне 1920×1200). */
const REEL_AREA = 164_157;

/** Переданный ролик: MSE, играет со звуком (человек включил звук), короткий, как ролик ленты. */
const REEL_A: VideoCandidate = {
  ...DEFAULT_CANDIDATE_FACTS,
  document: "doc-feed",
  element: 1,
  media: { kind: "media_source", in_worker: false },
  playing: true,
  visible_area_px: REEL_AREA,
  duration_s: 21.3,
};
/** Следующий ролик ленты: страница запускает его без звука (Instagram без жеста). */
const REEL_B: VideoCandidate = { ...REEL_A, element: 2, muted: true, duration_s: 12.1 };
/** Ещё один ролик ленты. */
const REEL_C: VideoCandidate = { ...REEL_A, element: 3, duration_s: 42.2 };

const report = (candidate: VideoCandidate, others: readonly VideoCandidate[] = []): HeldVideoReport => ({
  kind: HELD_VIDEO_REPORT_KIND,
  element: candidate.element,
  candidates: [candidate, ...others],
  trigger: "next_video",
});

function feedWorld(): World {
  return setup([{ frameId: 0, result: [REEL_A, { ...REEL_B, playing: false, visible_area_px: 0 }] }]);
}

function begins(world: World): Extract<PageCommand, { kind: "begin_handoff" }>[] {
  return world.framePorts.last().posted.filter((command): command is Extract<PageCommand, { kind: "begin_handoff" }> => command.kind === "begin_handoff");
}

/** Лента пролистана: страница поставила наш ролик на паузу, тень донесла уход с экрана (порядок живого Instagram). */
function scrolledAway(world: World): void {
  const port = world.framePorts.last();
  port.deliver({ kind: "page_playback", request: { kind: "pause" } });
  port.deliver({ kind: "video_left_screen", exit: "scrolled_off_screen" });
}

/** Новый документ подтвердил тень после переезда. */
function confirmMove(world: World): void {
  const begin = begins(world).at(-1);
  assert.ok(begin !== undefined, "begin_handoff не отправлен");
  world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
}

describe("лента: ушёл с экрана и остановлен страницей — переезд (решения 354, 359)", () => {
  it("Instagram: следующий тихий ролик принимается, окно одно, прежний ролик отпущен", async () => {
    const world = feedWorld();
    await startHandoff(world);
    const armingsBefore = world.armings.length;
    scrolledAway(world);
    assert.ok(world.armings.length > armingsBefore, "сторож взведён, чтобы успеть за play() следующего ролика");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(REEL_B)), "adopt", "тихий ролик ленты годится (решение 357)");
    const posted = world.framePorts.last().posted;
    assert.ok(posted.some((command) => command.kind === "end_handoff"), "прежняя тень снята");
    assert.equal(begins(world).at(-1)?.element, REEL_B.element);
    confirmMove(world);
    assert.equal(world.door.requests.length, 1, "окно одно: второго сеанса нет");
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.deepEqual(world.ended, []);
  });

  it("после переезда лента снова закрыта: следующий ролик ждёт, пока уйдёт с экрана уже новый", async () => {
    const world = feedWorld();
    await startHandoff(world);
    scrolledAway(world);
    world.handoffs.heldVideo(TAB, 0, report(REEL_B));
    confirmMove(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(REEL_C)), "release", "новый ролик на экране и играет");
    scrolledAway(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(REEL_C)), "adopt");
    assert.equal(begins(world).at(-1)?.element, REEL_C.element);
  });

  it("TikTok: страница вынула ушедший ролик — окно стоит на паузе и ждёт без предела (решения 355, 356)", async () => {
    const world = feedWorld();
    await startHandoff(world);
    scrolledAway(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.ok(world.sockets.last().wireSummary().includes("page_playback"), "плееру — пауза на последнем кадре");
    world.clock.advance(Math.max(NEXT_VIDEO_WAIT_MS, NEXT_EPISODE_WAIT_MS) * 20);
    assert.equal(world.sockets.last().closedWithCode, undefined, "срока у ленты нет: окно ждёт, пока человек его не закроет");
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(REEL_B)), "adopt", "следующий ролик через двадцать минут всё ещё принимается");
  });

  it("человек закрыл вкладку во время ожидания ленты — честный конец tab_gone", async () => {
    const world = feedWorld();
    await startHandoff(world);
    scrolledAway(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.handoffs.tabRemoved(TAB);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "closed" }]);
  });
});

describe("лента: что не переезжает (решение 359)", () => {
  it("наш ролик играет (ушёл к комментариям, видео идёт) — превью по наведению не уводит окно", async () => {
    const world = feedWorld();
    await startHandoff(world);
    // Тень не донесла `video_left_screen`: страница ролик не останавливала.
    assert.equal(world.handoffs.heldVideo(TAB, 0, report({ ...REEL_C, duration_s: 600 })), "release");
    assert.equal(begins(world).length, 1, "переезда не было");
  });

  it("страница снова запустила наш ролик — к нему вернулись, лента закрыта", async () => {
    const world = feedWorld();
    await startHandoff(world);
    scrolledAway(world);
    world.framePorts.last().deliver({ kind: "page_playback", request: { kind: "play" } });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(REEL_B)), "release");
  });

  it("невидимый следующий ролик не принимается даже на ленте", async () => {
    const world = feedWorld();
    await startHandoff(world);
    scrolledAway(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report({ ...REEL_B, visible_area_px: 0 })), "release");
  });

  it("без ухода с экрана страница, вынувшая ролик посреди просмотра, забрала его — 4009 (решение 179)", async () => {
    const world = feedWorld();
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
  });
});
