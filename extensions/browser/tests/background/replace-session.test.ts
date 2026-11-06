// Замена видео на той же вкладке (сессия 10b, решение 215) и порядок «передача идёт, попап закрылся».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { HANDOFF_END_REPLY_TIMEOUT_MS } from "../../src/background/handoff/constants.ts";
import { routeStatusPort, type StatusPortLike } from "../../src/background/status-port-router.ts";
import type { BridgeStatusProvider } from "../../src/background/status-provider.ts";
import type { PageCommand, VideoCandidate, VideoRef } from "../../src/shared/handoff-contract.ts";
import { PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

function video(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { ...PLAYING_VIDEO, element, ...overrides };
}

const refOf = (element: number): VideoRef => ({ frame_id: 0, document: PLAYING_VIDEO.document, element });
const REF_A = refOf(1);
const REF_B = refOf(2);

/** Играет A (главное), B стоит и тоже видно. */
const TWO_VIDEOS = [{ frameId: 0, result: [video(1), video(2, { playing: false, visible_area_px: 5000 })] }];

/** Сеанс A запущен и играет; возвращает его сокет и порт (потом их перекроет новый сеанс). */
async function startOnA(world: World) {
  const outcome = await startHandoff(world);
  assert.deepEqual(outcome, { kind: "started" });
  return { socket: world.sockets.last(), port: world.framePorts.last() };
}

/** Плеер принял новый сеанс (open_session выдан, WebSocket открыт, attach принят). */
async function attachReplacement(world: World, socketIndex = 1) {
  await flushPromises();
  const socket = world.sockets.sockets[socketIndex];
  assert.ok(socket !== undefined, "второй сеанс не открывался");
  socket.open();
  socket.receive({ type: "attached" });
  await flushPromises();
  return socket;
}

const endCommandsOf = (commands: readonly PageCommand[]) => commands.filter((command) => command.kind === "end_handoff");

/** Старый сеанс и его страница остались нетронутыми и играют. */
function assertOldSessionUntouched(
  world: World,
  old: { socket: { closedWithCode: number | undefined }; port: { posted: PageCommand[]; disconnectedByBackground: boolean } },
  endedSessions: readonly unknown[] = [],
) {
  assert.deepEqual(endCommandsOf(old.port.posted), [], "тень старого сеанса не снималась");
  assert.equal(old.port.disconnectedByBackground, false);
  assert.equal(old.socket.closedWithCode, undefined, "сокет старого сеанса не закрывался");
  assert.deepEqual(world.ended, endedSessions, "старый сеанс не кончался");
  assert.equal(world.door.demandHeld(), 1);
  assert.deepEqual(world.handoffs.activeTabIds(), [TAB]);
}

describe("замена видео на той же вкладке (решение 215)", () => {
  it("порядок: плеер принял новый сеанс → старый сеанс кончился → только потом begin_handoff нового", async () => {
    const world = setup(TWO_VIDEOS);
    const old = await startOnA(world);
    const replacement = world.handoffs.handOffTab(TAB, undefined, REF_B);
    const newSocket = await attachReplacement(world);
    assert.equal(world.door.requests.length, 2);
    assert.equal(world.door.requests[1]?.session_key, `t${TAB}-2-test`, "новый сеанс — с новым ключом");
    assert.equal(world.framePorts.ports.length, 1, "пока старый сеанс жив, страницу нового не трогаем: иначе тень старого закрылась бы «как тень»");

    // Плеер заменил окно: прежнему сокету — return_to_page на той же секунде и на паузе.
    old.socket.receive({ type: "return_to_page", position_us: 7_000_000, resume: "paused" });
    assert.deepEqual(old.port.posted.at(-1), { kind: "end_handoff", position: { kind: "at", position_us: 7_000_000 }, resume: { kind: "exactly", resume: "paused" } });
    await flushPromises();
    assert.equal(world.framePorts.ports.length, 1, "страница ещё не подтвердила снятие тени");

    old.port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }], "конец старого сеанса не потерян");
    assert.equal(world.framePorts.ports.length, 2);
    const begin = world.framePorts.last().posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    assert.equal(begin.element, 2, "новый сеанс начинается на выбранном видео");

    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await replacement, { kind: "started" });
    assert.notEqual(newSocket, old.socket);
    // Удаление старой записи не стёрло новую: вкладка по-прежнему со своим сеансом, в плеере B.
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB]);
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "in_player", video: REF_B });
    assert.equal(world.door.demandHeld(), 1, "нужда связи осталась только у нового сеанса");
  });

  it("старый сеанс не кончился за предел снятия тени — новый всё равно начинается", async () => {
    const world = setup(TWO_VIDEOS);
    await startOnA(world);
    void world.handoffs.handOffTab(TAB, undefined, REF_B);
    await attachReplacement(world);
    assert.equal(world.framePorts.ports.length, 1);
    world.clock.advance(HANDOFF_END_REPLY_TIMEOUT_MS);
    await flushPromises();
    assert.equal(world.framePorts.ports.length, 2, "зависший старый сеанс нового не держит");
  });

  it("то же видео, что уже в плеере, — сразу started, ничего не трогаем", async () => {
    const world = setup(TWO_VIDEOS);
    const old = await startOnA(world);
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, REF_A), { kind: "started" });
    assert.equal(world.door.requests.length, 1, "новый сеанс не открывался");
    assertOldSessionUntouched(world, old);
  });

  it("без video при живом сеансе — исход той же передачи, как раньше", async () => {
    const world = setup(TWO_VIDEOS);
    const old = await startOnA(world);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "started" });
    assert.equal(world.door.requests.length, 1);
    assertOldSessionUntouched(world, old);
  });

  it("отказ на любом шаге до сеанса оставляет старый сеанс нетронутым: устаревшая ссылка, DRM, форма, адрес", async () => {
    const localFile = video(5, { media: { kind: "file" }, file_host: "192.168.1.20", playing: false, visible_area_px: 4000 });
    const world = setup([
      {
        frameId: 0,
        result: [
          video(1),
          video(3, { drm: true, playing: false }),
          video(4, { media: { kind: "unsupported", form: "media_stream" }, playing: false }),
          localFile,
        ],
      },
    ]);
    const old = await startOnA(world);
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(99)), { kind: "video_gone" });
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(3)), { kind: "protected" });
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(4)), { kind: "not_supported_yet", form: "media_stream" });
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(5)), { kind: "local_address_refused" });
    assert.equal(world.door.requests.length, 1, "плеер ради отказов не будили");
    assertOldSessionUntouched(world, old);
    // Старый сеанс по-прежнему главный для вкладки.
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "in_player", video: REF_A });
  });

  it("плеер отказал в новом сеансе: старый жив, остаётся записью вкладки и снова главным", async () => {
    const world = setup(TWO_VIDEOS);
    const old = await startOnA(world);
    world.door.nextOutcome = () => ({
      kind: "refused",
      refusal: { type: "refused", in_reply_to: { kind: "open_session", request_id: 2 }, reason: { kind: "player_not_responding" } },
    });
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, REF_B), { kind: "player_unavailable", reason: "player_not_responding" });
    // Кончился только неудавшийся новый сеанс.
    assertOldSessionUntouched(world, old, [{ kind: "start_failed", outcome: { kind: "player_unavailable", reason: "player_not_responding" } }]);
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "in_player", video: REF_A });
  });

  it("закрытие вкладки во время замены доходит до обоих сеансов: оба кончаются, страница нового не тронута", async () => {
    const world = setup(TWO_VIDEOS);
    const old = await startOnA(world);
    const replacement = world.handoffs.handOffTab(TAB, undefined, REF_B);
    const newSocket = await attachReplacement(world);
    world.handoffs.tabRemoved(TAB);
    await flushPromises();
    assert.deepEqual(await replacement, { kind: "failed", reason: "tab_closed" });
    assert.deepEqual(old.socket.wireSummary().at(-1), "tab_gone", "старый сеанс узнал о закрытии");
    assert.deepEqual(newSocket.wireSummary(), ["attach", "tab_gone"], "новый сеанс честно закрыт через tab_gone");
    assert.equal(world.framePorts.ports.length, 1, "begin_handoff нового так и не ушёл");
    assert.equal(world.ended.length, 2, "конец каждого сеанса дошёл до владельца");
    assert.equal(world.door.demandHeld(), 0);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
  });

  it("команда с video, пока передача ещё стартует, ждёт её исход: то же видео — started без второго сеанса", async () => {
    const world = setup(TWO_VIDEOS);
    const first = world.handoffs.handOffTab(TAB);
    await flushPromises();
    const again = world.handoffs.handOffTab(TAB, undefined, REF_A);
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    const begin = world.framePorts.last().posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await first, { kind: "started" });
    assert.deepEqual(await again, { kind: "started" });
    assert.equal(world.door.requests.length, 1);
  });

  it("команда с другим video, пока передача стартует, после её старта заменяет видео", async () => {
    const world = setup(TWO_VIDEOS);
    const first = world.handoffs.handOffTab(TAB);
    await flushPromises();
    void world.handoffs.handOffTab(TAB, undefined, REF_B);
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    const begin = world.framePorts.last().posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await first, { kind: "started" });
    await flushPromises();
    assert.equal(world.door.requests.length, 2, "после старта первого сеанса открылся второй");
  });
});

describe("передача идёт, попап закрылся", () => {
  it("порт попапа закрыт — нужда попапа снята, а сеанс дошёл до started и живёт на своей нужде", async () => {
    const world = setup(TWO_VIDEOS);
    // Статус-провайдер на той же двери, что и сеанс: нужда попапа и нужда сеанса считаются вместе.
    const provider: BridgeStatusProvider = {
      current: () => ({ state: "checking" }),
      subscribe: () => () => {},
      acquireDemand: (reason) => world.door.acquireDemand(reason),
    };
    let closePopup: () => void = () => {};
    const popupPort: StatusPortLike = {
      name: "bridge-status",
      postMessage: () => {},
      onDisconnect: { addListener: (listener) => void (closePopup = listener) },
    };
    routeStatusPort(popupPort, provider);
    assert.equal(world.door.demandHeld(), 1, "попап держит связь");

    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    assert.equal(world.door.demandHeld(), 2, "попап и стартующая передача");

    closePopup();
    assert.equal(world.door.demandHeld(), 1, "нужда попапа снята, нужда передачи держит связь");

    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    const begin = world.framePorts.last().posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await outcome, { kind: "started" }, "закрытие попапа передачу не отменило");
    assert.equal(world.sockets.last().closedWithCode, undefined, "сокет сеанса открыт");
    assert.equal(world.framePorts.last().disconnectedByBackground, false);
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB]);
    assert.equal(world.door.demandHeld(), 1, "сеанс живёт на собственной нужде");
    assert.deepEqual(world.ended, []);
  });
});
