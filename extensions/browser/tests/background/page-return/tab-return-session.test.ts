// Возврат страницы в сеансе передачи (сессия 11b2) на фоне целиком: `createTabHandoffs` с фейковыми часами,
// плеером, сокетом, портами и браузером. Проверяется провод к плееру — то, что увидит сторож плеера (11b1):
// `page_left` без закрытия, тишина в сироте, `page_returned` раньше нового поколения N+1, отказы с причиной.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { PROTOCOL_VERSION_1_3 } from "../../../src/protocol/version.ts";
import { SESSION_CLOSE_CODES } from "../../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport, type PageEvent, type VideoCandidate } from "../../../src/shared/handoff-contract.ts";
import { deliverStream, MP4, PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "../handoff-world.ts";
import { flushPromises, grantFor, type FakeWebSocket } from "../support.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser, REOPENED_TAB, type FakePageReopen } from "./fake-page-reopen.ts";

const TAB_URL = "https://www.example.com/watch?v=1";
const FILM_DURATION_S = PLAYING_VIDEO.duration_s ?? 0;

/** Видео вернувшейся страницы: тот же фильм в новом документе. */
const RETURNED_FILM: VideoCandidate = { ...PLAYING_VIDEO, document: "doc-new", element: 9 };

function heldReport(candidate: VideoCandidate): HeldVideoReport {
  return { kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate] };
}

function clock(socket: FakeWebSocket, seq: number, positionS: number): void {
  socket.receive({ type: "clock", seq, position_us: positionS * 1_000_000, state: "playing", rate: 1000 });
}

/** Передача идёт, поколение 1 отдано, плеер на 61-й секунде. */
async function handedOff(options: { readonly protocol?: typeof PROTOCOL_VERSION_1_3 } = {}) {
  const reopen: FakePageReopen = createFakePageReopenBrowser({ url: TAB_URL, windowId: BROWSER_WINDOW, incognito: false });
  const world: World = setup(undefined, undefined, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser });
  if (options.protocol !== undefined) {
    const protocol = options.protocol;
    world.door.nextOutcome = (request) => grantFor(request, "started", protocol);
  }
  await startHandoff(world);
  deliverStream(world);
  const socket = world.sockets.last();
  clock(socket, 1, 61);
  return { world, reopen, socket };
}

/** Вкладку закрыли: порт умер раньше `tabs.onRemoved`, как в браузере. */
async function closeSourceTab(world: World): Promise<void> {
  world.closeTab();
  world.framePorts.last().drop();
  world.handoffs.tabRemoved(TAB);
  await flushPromises();
}

/** Сторож новой вкладки задержал фильм, фон его взял, тень встала. */
async function returnedShadowStands(world: World): Promise<void> {
  assert.equal(world.handoffs.heldVideo(REOPENED_TAB, 0, heldReport(RETURNED_FILM)), "adopt");
  const port = world.framePorts.last();
  assert.equal(port.tabId, REOPENED_TAB, "порт открыт к вернувшейся вкладке");
  const begin = port.posted.find((command) => command.kind === "begin_handoff");
  assert.ok(begin?.kind === "begin_handoff");
  assert.equal(begin.document, RETURNED_FILM.document);
  port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" } satisfies PageEvent);
  await flushPromises();
}

const typesAfter = (socket: FakeWebSocket, type: string): string[] => {
  const wire = socket.wireSummary();
  return wire.slice(wire.lastIndexOf(type) + 1);
};

describe("сирота: страница ушла, связь держим", () => {
  it("закрытая вкладка — `page_left{closed}`, сокет открыт, после него к плееру ни байта", async () => {
    const { world, socket } = await handedOff();
    await closeSourceTab(world);
    assert.equal(socket.closedWithCode, undefined, "связь не закрыта");
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_left", reason: "closed" });
    assert.equal(socket.sentMessages().some((message) => message.type === "tab_gone"), false);
    // Плеер продолжает слать часы — сирота их принимает и молчит в ответ.
    clock(socket, 2, 62);
    clock(socket, 3, 63);
    await flushPromises();
    assert.deepEqual(typesAfter(socket, "page_left"), []);
    assert.deepEqual(world.ended, [], "сеанс жив");
  });

  it("плеер до 1.4 — прежний `tab_gone` и закрытие 4002", async () => {
    const { world, socket } = await handedOff({ protocol: PROTOCOL_VERSION_1_3 });
    await closeSourceTab(world);
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "closed" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
  });

  it("плеер сказал «полон» — придержанные рамки ушедшего поколения выбрасываются, а не уходят после `page_left`", async () => {
    const { world, socket } = await handedOff();
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    world.framePorts.last().deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([9, 9]) });
    await closeSourceTab(world);
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 1_000_000, intake: "accepting" });
    await flushPromises();
    assert.deepEqual(typesAfter(socket, "page_left"), [], "4003 у плеера не будет");
  });
});

describe("возврат: тот же сеанс в новой вкладке", () => {
  it("`reopen_page` — вкладка в окне страницы на ПОСЛЕДНЕЙ секунде часов; тень встала — `page_returned`, потом поколение 2", async () => {
    const { world, reopen, socket } = await handedOff();
    await closeSourceTab(world);
    clock(socket, 2, 75);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    assert.deepEqual(reopen.opened, [{ windowId: BROWSER_WINDOW, url: TAB_URL }]);
    assert.deepEqual(reopen.arms[0]?.target, { kind: "recorded", duration_s: FILM_DURATION_S, start_s: 75 }, "секунда часов, а не просьбы");
    // Пока страница грузится, плеер играет скачанное: следующий взвод несёт уже новую секунду.
    clock(socket, 3, 77);
    world.clock.advance(1_000);
    await flushPromises();
    assert.deepEqual(reopen.arms.at(-1)?.target, { kind: "recorded", duration_s: FILM_DURATION_S, start_s: 77 });

    await returnedShadowStands(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "page_seek", target_us: 75_000_000 });
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
    port.deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([4, 5, 6]) });
    await flushPromises();

    const after = typesAfter(socket, "page_left");
    assert.equal(after[0], "page_returned", "сначала «вернулась» — до любого события нового документа");
    const generations = socket.sentMessages().filter((message) => message.type === "generation_started");
    assert.deepEqual(
      generations.map((message) => message.generation),
      [1, 2],
      "новое поколение ровно на 1 больше последнего",
    );
    assert.ok(after.indexOf("generation_started") > 0);
    assert.ok(after.some((item) => item.startsWith("frame 2/")), "байты нового поколения идут плееру");
    assert.equal(socket.closedWithCode, undefined);
    assert.deepEqual(reopen.muted, [{ tabId: REOPENED_TAB, muted: false }], "вкладка снова со звуком");
    assert.deepEqual(reopen.closed, []);

    // Плеер командует уже новой вкладкой: часы доходят до её тени.
    clock(socket, 3, 76);
    assert.ok(port.posted.some((command) => command.kind === "shadow" && command.command.kind === "clock"));
  });

  it("вернувшаяся страница может уйти снова: новая сирота, новая просьба", async () => {
    const { world, reopen, socket } = await handedOff();
    await closeSourceTab(world);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    await returnedShadowStands(world);
    world.framePorts.last().drop();
    world.handoffs.tabRemoved(REOPENED_TAB);
    await flushPromises();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_left", reason: "closed" });
    socket.receive({ type: "reopen_page", position_us: 70_000_000 });
    await flushPromises();
    assert.equal(reopen.opened.length, 2);
  });

  it("одна просьба на сироту: повторный `reopen_page` ничего не открывает", async () => {
    const { world, reopen, socket } = await handedOff();
    await closeSourceTab(world);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    socket.receive({ type: "reopen_page", position_us: 62_000_000 });
    await flushPromises();
    assert.equal(reopen.opened.length, 1);
  });

  it("видео другой длины фон не берёт; до просьбы донесения сторожа тоже отпускаются", async () => {
    const { world, socket } = await handedOff();
    await closeSourceTab(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport(RETURNED_FILM)), "release", "до просьбы");
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    const preroll = { ...RETURNED_FILM, duration_s: 15 };
    assert.equal(world.handoffs.heldVideo(REOPENED_TAB, 0, heldReport(preroll)), "release");
  });
});

describe("возврат: отказы и отмена", () => {
  it("окна нужного вида нет — `page_reopen_failed{no_browser_window}`, затем 4002", async () => {
    const { world, reopen, socket } = await handedOff();
    reopen.behavior.window = undefined;
    await closeSourceTab(world);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_reopen_failed", reason: "no_browser_window" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.deepEqual(world.ended, [{ kind: "page_not_returned", reason: "no_browser_window" }]);
  });

  it("зритель закрыл открытую нами вкладку до тени — `page_did_not_load`", async () => {
    const { world, socket } = await handedOff();
    await closeSourceTab(world);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    world.handoffs.tabRemoved(REOPENED_TAB);
    await flushPromises();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_reopen_failed", reason: "page_did_not_load" });
  });

  it("тень вернувшейся страницы не встала (элемент пропал) — `video_did_not_start`, наша вкладка закрыта", async () => {
    const { world, reopen, socket } = await handedOff();
    await closeSourceTab(world);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    world.handoffs.heldVideo(REOPENED_TAB, 0, heldReport(RETURNED_FILM));
    const port = world.framePorts.last();
    const begin = port.posted.find((command) => command.kind === "begin_handoff");
    assert.ok(begin?.kind === "begin_handoff");
    port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "element_not_found" } satisfies PageEvent);
    await flushPromises();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_reopen_failed", reason: "video_did_not_start" });
    assert.deepEqual(reopen.closed, [REOPENED_TAB]);
  });

  it("окно плеера закрыто во время открытия (4000) — открытие отменено, наша вкладка закрыта", async () => {
    const { world, reopen, socket } = await handedOff();
    await closeSourceTab(world);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    socket.remoteClose(SESSION_CLOSE_CODES.player_closed);
    await flushPromises();
    assert.deepEqual(reopen.closed, [REOPENED_TAB]);
    assert.equal(world.ended.at(-1)?.kind, "player_ended");
    assert.equal(world.handoffs.heldVideo(REOPENED_TAB, 0, heldReport(RETURNED_FILM)), "release", "сеанса больше нет");
  });

  it("окно плеера закрыто в сироте, до просьбы — сеанс кончается, вкладок никто не открывает", async () => {
    const { world, reopen, socket } = await handedOff();
    await closeSourceTab(world);
    socket.remoteClose(SESSION_CLOSE_CODES.player_closed);
    await flushPromises();
    assert.equal(world.ended.at(-1)?.kind, "player_ended");
    assert.deepEqual(reopen.opened, []);
    assert.equal(world.door.demandHeld(), 0, "нужда связи освобождена");
  });
});
