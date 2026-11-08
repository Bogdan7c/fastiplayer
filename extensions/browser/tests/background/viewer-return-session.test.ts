// Концы сеанса сессии 11a через весь фон (createTabHandoffs на фейках):
// - «Return» с заглушки (решение 236): тень на её позиции, страница играет, окно закрыто 4010 сразу;
// - окно закрыто зрителем без прощания (решение 238): страница на паузе на позиции тени;
// - сломался плеер страницы (решение 237): как уход вкладки, причина по версии протокола.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { HANDOFF_END_REPLY_TIMEOUT_MS } from "../../src/background/handoff/constants.ts";
import { PROTOCOL_VERSION_1_2, PROTOCOL_VERSION_1_3, SESSION_CLOSE_CODES, type ProtocolVersion } from "../../src/protocol/index.ts";
import { VIEWER_RETURN_REQUEST_KIND, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises, grantFor } from "./support.ts";

const VIEWER_RETURN = { kind: VIEWER_RETURN_REQUEST_KIND };
const RETURN_PLAYING: PageCommand = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "exactly", resume: "playing" } };
const RETURN_PAUSED: PageCommand = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "exactly", resume: "paused" } };
const END_AS_SHADOW: PageCommand = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } };

/** Реклама и фильм (форма Ashdi): сеанс начинается на рекламе, переезд открыт. */
const AD: VideoCandidate = {
  ...DEFAULT_CANDIDATE_FACTS,
  document: "doc-ad",
  element: 3,
  media: { kind: "file" },
  file_host: "video.unknown-ad-cdn.example",
  playing: true,
  visible_area_px: 296_000,
  duration_s: 24,
};
const AD_FILM: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 296_000, duration_s: 6381 };

const endCommands = (world: World): PageCommand[] => world.framePorts.last().posted.filter((command) => command.kind === "end_handoff");

describe("«Return» с заглушки", () => {
  it("в активной передаче: тень снимается на её позиции и страница играет; окно закрыто 4010 сразу, без tab_gone", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    const socket = world.sockets.last();
    port.deliver(VIEWER_RETURN);
    assert.deepEqual(endCommands(world), [RETURN_PLAYING]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.viewer_returned_to_page, "окно закрыто, не дожидаясь страницы");
    assert.deepEqual(world.ended, [], "конец сообщается по подтверждению страницы");
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "returned_by_viewer" }]);
    assert.equal(socket.wireSummary().includes("tab_gone"), false);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.equal(await world.tabModel.lastRefusalOf(TAB), null, "выбор зрителя — не отказ");
    assert.equal(world.door.demandHeld(), 0);
  });

  it("страница не подтвердила снятие вовремя — сеанс всё равно кончается тем же концом, ровно один раз", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver(VIEWER_RETURN);
    world.clock.advance(HANDOFF_END_REPLY_TIMEOUT_MS);
    await flushPromises();
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "returned_by_viewer" }]);
  });

  it("двойное нажатие: вторая просьба ничего не меняет", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver(VIEWER_RETURN);
    world.framePorts.last().deliver(VIEWER_RETURN);
    assert.deepEqual(endCommands(world), [RETURN_PLAYING]);
  });

  it("окно закрыли раньше (return_to_page плеера уже в работе) — «Return» не шлёт второй end_handoff, конец плеера", async () => {
    const world = setup();
    await startHandoff(world);
    const socket = world.sockets.last();
    socket.receive({ type: "return_to_page", position_us: 5_000_000, resume: "paused" });
    world.framePorts.last().deliver(VIEWER_RETURN);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(endCommands(world), [{ kind: "end_handoff", position: { kind: "at", position_us: 5_000_000 }, resume: { kind: "exactly", resume: "paused" } }]);
    assert.equal(socket.closedWithCode, 1000);
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
  });

  it("«Return» первым — поздний return_to_page и закрытие сокета плеером конец не меняют", async () => {
    const world = setup();
    await startHandoff(world);
    const socket = world.sockets.last();
    world.framePorts.last().deliver(VIEWER_RETURN);
    socket.receive({ type: "return_to_page", position_us: 9_000_000, resume: "paused" });
    socket.remoteClose(4000);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(endCommands(world), [RETURN_PLAYING]);
    assert.deepEqual(world.ended, [{ kind: "returned_by_viewer" }]);
  });

  it("между роликами тени нет — просьба ничего не делает, сеанс ждёт следующее видео", async () => {
    const world = setup([{ frameId: 0, result: [AD_FILM, AD] }]);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    const before = world.framePorts.last().posted.length;
    world.framePorts.last().deliver(VIEWER_RETURN);
    assert.equal(world.framePorts.last().posted.length, before, "странице ничего не ушло");
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.deepEqual(world.ended, []);
  });

  it("просьба не по форме — не просьба", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: VIEWER_RETURN_REQUEST_KIND, document: "doc-7f3a" });
    assert.deepEqual(endCommands(world), []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
  });
});

describe("окно закрыто зрителем без прощания (решение 238)", () => {
  for (const code of [4000, 4001, 4006]) {
    it(`код ${code}: страница остаётся на паузе на позиции тени`, async () => {
      const world = setup();
      await startHandoff(world);
      world.sockets.last().remoteClose(code);
      assert.deepEqual(endCommands(world), [RETURN_PAUSED]);
    });
  }

  for (const code of [1006, 4003, 1000]) {
    it(`обрыв ${code}: как прежде — позиция тени, играть как тень`, async () => {
      const world = setup();
      await startHandoff(world);
      world.sockets.last().remoteClose(code);
      assert.deepEqual(endCommands(world), [END_AS_SHADOW]);
    });
  }
});

describe("сломался плеер страницы (решение 237)", () => {
  async function failedPagePlayer(agreed: ProtocolVersion) {
    const world = setup();
    world.door.nextOutcome = (request) => grantFor(request, "started", agreed);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "page_player_failed" });
    await flushPromises();
    return world;
  }

  it("плеер 1.3: tab_gone{page_player_failed} и 4002 — доиграть скачанное", async () => {
    const world = await failedPagePlayer(PROTOCOL_VERSION_1_3);
    const socket = world.sockets.last();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "page_player_failed" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "page_player_failed" }]);
  });

  it("плеер 1.2 нового значения не знает: tab_gone{navigated_away}", async () => {
    const world = await failedPagePlayer(PROTOCOL_VERSION_1_2);
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("ошибка рекламы, когда ждём фильм: сеанс не кончается, а ждёт следующее видео (как element_released)", async () => {
    const world = setup([{ frameId: 0, result: [AD_FILM, AD] }]);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "page_player_failed" });
    await flushPromises();
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.deepEqual(world.ended, []);
    assert.ok(world.sockets.last().wireSummary().includes("page_playback"), "плееру — пауза до следующего ролика");
  });
});
