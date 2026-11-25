// Окно плеера досматривает видео ПРЕЖНЕЙ страницы, а вкладка уже на другом сайте (бэклог п. 75, решение 395,
// сессия 16b5). Сеанс намеренно жив «сиротой» (11b2): связь окна держится, чтобы страницу можно было вернуть.
// Проверяется то, что увидят попап, плеер и страница: вкладка больше не считается «в плеере», а кнопка без
// выбранного видео отдаёт главное видео новой страницы в то же окно заменой сеанса (как строка списка, 215).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { SESSION_CLOSE_CODES, type PageSessionMessage } from "../../src/protocol/index.ts";
import type { PageCommand, VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { deliverStream, PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser, REOPENED_TAB } from "./page-return/fake-page-reopen.ts";
import { flushPromises, grantFor, type FakeWebSocket } from "./support.ts";

/** Адрес вкладки при старте (мир по умолчанию). */
const FIRST_SITE_URL = "https://www.example.com/watch?v=1";
const OTHER_SITE_URL = "https://other-site.test/series/7";
/** Главное видео страницы другого сайта. */
const OTHER_SITE_FILM: VideoCandidate = { ...PLAYING_VIDEO, document: "doc-other-site", element: 4 };

const sent = (socket: FakeWebSocket, type: PageSessionMessage["type"]): PageSessionMessage[] => socket.sentMessages().filter((message) => message.type === type);

interface Orphaned {
  readonly world: World;
  /** Сокет сеанса-сироты: окно плеера по нему досматривает прежнее видео. */
  readonly orphanSocket: FakeWebSocket;
  readonly reopen: ReturnType<typeof createFakePageReopenBrowser>;
}

/** Видео играло в окне, вкладка ушла на другой сайт: сеанс — сирота, новая страница показывает свой фильм. */
async function orphanedWindowOnAnotherSite(): Promise<Orphaned> {
  const reopen = createFakePageReopenBrowser({ url: FIRST_SITE_URL, windowId: BROWSER_WINDOW, incognito: false });
  const world = setup(undefined, undefined, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser });
  await startHandoff(world);
  deliverStream(world);
  const orphanSocket = world.sockets.last();
  orphanSocket.receive({ type: "clock", seq: 1, position_us: 1_000_000, state: "playing", rate: 1000 });
  world.updateTab({ url: OTHER_SITE_URL });
  world.framePorts.last().drop();
  await flushPromises();
  assert.deepEqual(sent(orphanSocket, "page_left"), [{ type: "page_left", reason: "navigated_away" }], "предусловие: сеанс стал сиротой");
  assert.equal(orphanSocket.closedWithCode, undefined, "предусловие: связь окна жива");
  assert.deepEqual(world.ended, [], "предусловие: сеанс не кончился");
  world.setFrames([{ frameId: 0, result: [OTHER_SITE_FILM] }]);
  return { world, orphanSocket, reopen };
}

/** Плеер принял сеанс-замену и закрыл связь прежнего (так он отвечает на второй `open_session`, решение 216). */
async function playerReplacesItsWindow(world: World, orphanSocket: FakeWebSocket): Promise<FakeWebSocket> {
  await flushPromises();
  const replacementSocket = world.sockets.sockets[1];
  assert.ok(replacementSocket !== undefined, "сеанс-замена не открывался");
  replacementSocket.open();
  replacementSocket.receive({ type: "attached" });
  await flushPromises();
  orphanSocket.receive({ type: "return_to_page", position_us: 9_000_000, resume: "paused" });
  orphanSocket.remoteClose(SESSION_CLOSE_CODES.replaced_by_another_tab);
  await flushPromises();
  return replacementSocket;
}

/** Новая страница подтвердила начало передачи своего фильма. */
function newPageBegins(world: World): Extract<PageCommand, { kind: "begin_handoff" }> {
  const port = world.framePorts.last();
  const begin = port.posted.find((command): command is Extract<PageCommand, { kind: "begin_handoff" }> => command.kind === "begin_handoff");
  assert.ok(begin !== undefined, "новой странице не пришёл begin_handoff");
  port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
  return begin;
}

describe("окно досматривает прежнюю страницу, вкладка на другом сайте (п. 75)", () => {
  it("опрос вкладки: видео этой страницы в плеере нет — попап не должен отвечать «Playing in Fastiplayer»", async () => {
    const { world } = await orphanedWindowOnAnotherSite();
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "previous_page_in_player" });
    assert.deepEqual(reply.main, { frame_id: 0, document: OTHER_SITE_FILM.document, element: OTHER_SITE_FILM.element }, "главное видео — у новой страницы");
  });

  // Той же командой без видео ходят пункт меню страницы (клик мимо видео) и поздний вход: чинятся вместе.
  it("кнопка без выбранного видео: главное видео новой страницы встаёт в то же окно заменой сеанса", async () => {
    const { world, orphanSocket } = await orphanedWindowOnAnotherSite();
    world.door.nextOutcome = (request) => grantFor(request, "replaced_previous");
    const pressed = world.handoffs.handOffTab(TAB);
    const replacementSocket = await playerReplacesItsWindow(world, orphanSocket);
    assert.equal(world.door.requests.length, 2, "плееру ушёл второй open_session");
    const begin = newPageBegins(world);
    assert.deepEqual({ document: begin.document, element: begin.element }, { document: OTHER_SITE_FILM.document, element: OTHER_SITE_FILM.element });
    assert.deepEqual(await pressed, { kind: "started", player_window: "already_open" });
    assert.notEqual(replacementSocket, orphanSocket);
    assert.equal(world.ended.length, 1, "сеанс-сирота кончился ровно один раз");
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB]);
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "in_player", video: { frame_id: 0, document: OTHER_SITE_FILM.document, element: OTHER_SITE_FILM.element } });
    assert.equal(world.door.demandHeld(), 1, "нужда связи осталась только у сеанса-замены");
  });

  it("на новой странице нет видео: честный отказ, сирота нетронута и окно досматривает дальше", async () => {
    const { world, orphanSocket } = await orphanedWindowOnAnotherSite();
    world.setFrames([{ frameId: 0, result: [] }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "no_video" });
    assert.equal(world.door.requests.length, 1, "плеер не будили");
    assert.equal(orphanSocket.closedWithCode, undefined, "связь окна жива");
    assert.deepEqual(world.ended, []);
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "previous_page_in_player" }, "возврат прежней страницы по-прежнему возможен");
  });

  it("кнопка во время возврата прежней страницы (reopen_page): замена побеждает, начатый возврат отменён", async () => {
    const { world, orphanSocket, reopen } = await orphanedWindowOnAnotherSite();
    orphanSocket.receive({ type: "reopen_page", position_us: 9_000_000 });
    await flushPromises();
    assert.equal(reopen.opened.length, 1, "предусловие: возврат начат — прежняя страница открывается в новой вкладке");
    world.door.nextOutcome = (request) => grantFor(request, "replaced_previous");
    const pressed = world.handoffs.handOffTab(TAB);
    await playerReplacesItsWindow(world, orphanSocket);
    newPageBegins(world);
    assert.deepEqual(await pressed, { kind: "started", player_window: "already_open" });
    assert.deepEqual(reopen.closed, [REOPENED_TAB], "вкладка начатого возврата закрыта: она никому не нужна");
    assert.equal(world.ended.length, 1);
  });
});

describe("живая страница: прежнее поведение кнопки не изменилось (решение 216)", () => {
  it("видео этой страницы в плеере — кнопка без видео отвечает «уже в окне» и ничего не открывает", async () => {
    const world = setup();
    await startHandoff(world);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "started", player_window: "already_open" });
    assert.equal(world.door.requests.length, 1);
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed" && reply.in_player.kind === "in_player");
  });
});
