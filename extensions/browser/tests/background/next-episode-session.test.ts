// Смена серии на той же странице в сеансе передачи целиком (сессия 16a, решения 303, 306–308, 42): фон на
// фейковых часах, плеере, сокете и порте. Проверяется провод к плееру — что он увидит и каким кодом
// закроется связь: «ждём следующую серию», 4011 и 4009 по версии плеера, заголовок вкладки, уход
// одностраничника `tab_gone{navigated_away}`.
import assert from "node:assert/strict";
import { PAGE_TITLE_AWAY_HOLD_MS, PAGE_TITLE_SETTLE_MS } from "../../src/background/handoff/episode-change/page-title-gate.ts";
import { describe, it } from "node:test";
import { NEXT_VIDEO_WAIT_MS } from "../../src/background/handoff/video-relocation.ts";
import { PROTOCOL_VERSION_1_8, SESSION_CLOSE_CODES, sanitizePageTitle, type PageSessionMessage, type ProtocolVersion } from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, SHADOW_SILENCE_LIMIT_MS, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../src/shared/next-episode-wait.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { deliverStream, MP4, PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises, grantFor } from "./support.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser, REOPENED_TAB } from "./page-return/fake-page-reopen.ts";

const SERIES_URL = "https://www.example.com/watch?v=1";
const AWAITING: PageSessionMessage = { type: "awaiting_next_episode", patience_ms: NEXT_EPISODE_WAIT_MS };
/** Заголовок, который фейковый браузер мира отдаёт при открытии сеанса (`describeTab`). */
const OPENED_RAW_TITLE = "Серія 3\nОзвучка";

/** Серия 2 — файл на том же или новом элементе (форма Tortuga после конца серии). */
const NEXT_EPISODE_FILE: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: PLAYING_VIDEO.document, element: 9, media: { kind: "file" }, playing: true, visible_area_px: 200_000, duration_s: 1500 };

async function series(options: { readonly protocol?: ProtocolVersion } = {}): Promise<World> {
  const world = setup();
  const protocol = options.protocol;
  if (protocol !== undefined) world.door.nextOutcome = (request) => grantFor(request, "started", protocol);
  await startHandoff(world);
  deliverStream(world);
  return world;
}

function clock(world: World, state: "playing" | "ended", seq: number): void {
  world.sockets.last().receive({ type: "clock", seq, position_us: 1_400_000_000, state, rate: 1000 });
}

function sent(world: World, type: PageSessionMessage["type"]): PageSessionMessage[] {
  return world.sockets.last().sentMessages().filter((message) => message.type === type);
}

/** Тень на том же элементе начала ждать следующую серию (донесение страницы). */
function shadowWaitsForNextEpisode(world: World): void {
  world.framePorts.last().deliver({ kind: "next_episode_wait_began" });
}

/** Страница начала поколение серии 2 тем же элементом (новый MediaSource). */
function secondGeneration(world: World): void {
  world.framePorts.last().deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-2", source_buffer: 0, mime_type: MP4 } });
}

const heldNextFile = (): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: NEXT_EPISODE_FILE.element, candidates: [NEXT_EPISODE_FILE], trigger: "next_video" });

describe("ждём следующую серию на том же элементе (YouTube)", () => {
  it("конец серии у плеера и донесение тени — `awaiting_next_episode{60000}` ровно один раз", async () => {
    const world = await series();
    clock(world, "ended", 1);
    shadowWaitsForNextEpisode(world);
    shadowWaitsForNextEpisode(world);
    assert.deepEqual(sent(world, "awaiting_next_episode"), [AWAITING]);
  });

  it("две серии подряд — два слова: новое поколение начинает новую серию", async () => {
    const world = await series();
    clock(world, "ended", 1);
    shadowWaitsForNextEpisode(world);
    secondGeneration(world);
    clock(world, "playing", 2);
    clock(world, "ended", 3);
    shadowWaitsForNextEpisode(world);
    assert.deepEqual(sent(world, "awaiting_next_episode"), [AWAITING, AWAITING]);
  });

  it("страница говорит «жду следующую», а наши часы конца не видели — плееру ничего (странице не верим)", async () => {
    const world = await series();
    clock(world, "playing", 1);
    shadowWaitsForNextEpisode(world);
    assert.deepEqual(sent(world, "awaiting_next_episode"), []);
  });

  it("старый плеер (1.8): ни слова, а конец ожидания — прежний 4009", async () => {
    const world = await series({ protocol: PROTOCOL_VERSION_1_8 });
    clock(world, "ended", 1);
    shadowWaitsForNextEpisode(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "next_episode_not_started" });
    assert.deepEqual(sent(world, "awaiting_next_episode"), []);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(world.ended, [{ kind: "next_episode_not_started" }]);
  });

  it("тень не дождалась (60 с у неё) — сразу 4011 после очереди, без tab_gone и без второго ожидания", async () => {
    const world = await series();
    clock(world, "ended", 1);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "next_episode_not_started" });
    const socket = world.sockets.last();
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.next_episode_not_started);
    assert.equal(socket.closedWithCode, 4011);
    assert.equal(socket.wireSummary().includes("tab_gone"), false);
    assert.deepEqual(world.ended, [{ kind: "next_episode_not_started" }]);
  });
});

describe("переезд после конца серии (Tortuga: новый файл тем же или новым элементом)", () => {
  /** Серия доиграла, страница отдала элемент (переписала файл): сеанс между видео. */
  async function episodeEndedAndElementReleased(protocol?: ProtocolVersion): Promise<World> {
    const world = await series(protocol === undefined ? {} : { protocol });
    clock(world, "ended", 1);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    return world;
  }

  it("ждём 60 с, а не 15: «ждём» уходит один раз; ровно на сроке — 4011", async () => {
    const world = await episodeEndedAndElementReleased();
    assert.deepEqual(sent(world, "awaiting_next_episode"), [AWAITING]);
    world.clock.advance(NEXT_VIDEO_WAIT_MS);
    assert.deepEqual(world.ended, [], "15 с прежнего срока — ещё ждём");
    world.clock.advance(NEXT_EPISODE_WAIT_MS - NEXT_VIDEO_WAIT_MS - 1);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.next_episode_not_started);
    assert.deepEqual(world.ended, [{ kind: "next_episode_not_started" }]);
    assert.equal(sent(world, "awaiting_next_episode").length, 1);
  });

  it("часы плеера между видео сменились на `playing` — срок и конец остаются от конца серии", async () => {
    const world = await episodeEndedAndElementReleased();
    clock(world, "playing", 2);
    world.clock.advance(NEXT_EPISODE_WAIT_MS - 1);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.deepEqual(world.ended, [{ kind: "next_episode_not_started" }]);
  });

  it("серия 2 пришла на 50-й секунде — переезд, сеанс жив и после срока", async () => {
    const world = await episodeEndedAndElementReleased();
    world.clock.advance(50_000);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldNextFile()), "adopt");
    const begin = world.framePorts.last().posted.filter((command): command is Extract<PageCommand, { kind: "begin_handoff" }> => command.kind === "begin_handoff").at(-1);
    assert.ok(begin !== undefined);
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    // Новая тень жива и подаёт пульс: сеанс мог бы кончить только старый срок ожидания — его больше нет.
    for (let waitedMs = 0; waitedMs < 2 * NEXT_EPISODE_WAIT_MS; waitedMs += SHADOW_SILENCE_LIMIT_MS / 2) {
      world.framePorts.last().deliver({ kind: "shadow_alive" });
      world.clock.advance(SHADOW_SILENCE_LIMIT_MS / 2);
    }
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
  });

  it("старый плеер (1.8): тот же срок 60 с, но без слова и с прежним 4009", async () => {
    const world = await episodeEndedAndElementReleased(PROTOCOL_VERSION_1_8);
    world.clock.advance(NEXT_EPISODE_WAIT_MS - 1);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.deepEqual(sent(world, "awaiting_next_episode"), []);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(world.ended, [{ kind: "next_episode_not_started" }]);
  });
});

describe("одностраничник ушёл без видео (решение 42)", () => {
  it("адрес сменился, видео пропало посреди серии — tab_gone{navigated_away}, а не 4009", async () => {
    const world = await series();
    world.updateTab({ url: "https://www.example.com/" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    await flushPromises();
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("адрес тот же — прежние 4009 «страница забрала видео»", async () => {
    const world = await series();
    world.updateTab({ url: SERIES_URL, title: "Серія 3" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
  });

  it("серия 2 началась на новом адресе — он опорный: её честный конец посреди — снова 4009", async () => {
    const world = await series();
    world.updateTab({ url: "https://www.example.com/watch?v=2" });
    secondGeneration(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
  });

  it("серия доиграла, а одностраничник ушёл на главную — уход, а не 4011", async () => {
    const world = await series();
    clock(world, "ended", 1);
    world.updateTab({ url: "https://www.example.com/" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "next_episode_not_started" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("переезд после конца серии истёк, адрес сменился — уход", async () => {
    const world = await series();
    clock(world, "ended", 1);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.updateTab({ url: "https://www.example.com/feed" });
    world.clock.advance(NEXT_EPISODE_WAIT_MS);
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });
});

describe("заголовок вкладки → page_title_changed", () => {
  it("новая серия: очищенный заголовок уходит; повтор и заголовок открытия — нет; чужая вкладка — нет", async () => {
    const world = await series();
    world.updateTab({ title: OPENED_RAW_TITLE });
    world.updateTab({ tabId: TAB + 1, title: "Чужая вкладка" });
    world.updateTab({ title: "Серія 4\nОзвучка" });
    world.updateTab({ title: "Серія 4\nОзвучка" });
    assert.deepEqual(sent(world, "page_title_changed"), [], "заголовок ждёт адрес той же навигации (16a3)");
    world.clock.advance(PAGE_TITLE_SETTLE_MS);
    assert.deepEqual(sent(world, "page_title_changed"), [{ type: "page_title_changed", title: sanitizePageTitle("Серія 4\nОзвучка") }]);
  });

  it("уход на главную одностраничника (16a3): заголовок главной до окна не доходит, сеанс кончается уходом", async () => {
    const world = await series();
    // Firefox сообщил заголовок главной раньше её адреса; через 5 с тень отпустила элемент.
    world.updateTab({ title: "(327) YouTube" });
    world.updateTab({ url: "https://www.example.com/" });
    world.clock.advance(5_000);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    await flushPromises();
    world.clock.advance(PAGE_TITLE_SETTLE_MS * 5);
    assert.deepEqual(sent(world, "page_title_changed"), [], "окно досматривает прежний ролик под его названием");
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
  });

  it("новый ролик одностраничника (16a3): адрес и заголовок сменились — заголовок уходит с началом нового поколения", async () => {
    const world = await series();
    world.updateTab({ url: "https://www.example.com/watch?v=2" });
    world.updateTab({ title: "Ролик 2" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS * 3);
    assert.deepEqual(sent(world, "page_title_changed"), [], "старый ролик ещё на экране");
    secondGeneration(world);
    assert.deepEqual(sent(world, "page_title_changed"), [{ type: "page_title_changed", title: "Ролик 2" }]);
    const wire = world.sockets.last().wireSummary();
    assert.ok(wire.indexOf("generation_started") < wire.lastIndexOf("page_title_changed"), `the title follows the new generation: ${wire.join()}`);
  });

  it("старый плеер (1.8) — ни слова", async () => {
    const world = await series({ protocol: PROTOCOL_VERSION_1_8 });
    world.updateTab({ title: "Серія 4" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS);
    assert.deepEqual(sent(world, "page_title_changed"), []);
  });

  it("конец сеанса снимает подписку на вкладки", async () => {
    const world = await series();
    assert.equal(world.tabUpdateSubscribers(), 1);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(world.tabUpdateSubscribers(), 0);
  });

  it("страница ушла сиротой и возвращается — ни слова; тень встала в новой вкладке — слово снова идёт", async () => {
    const reopen = createFakePageReopenBrowser({ url: SERIES_URL, windowId: BROWSER_WINDOW, incognito: false });
    const world = setup(undefined, undefined, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser });
    await startHandoff(world);
    deliverStream(world);
    const socket = world.sockets.last();
    // Вкладку закрыли: сирота, `page_left`.
    world.closeTab();
    world.framePorts.last().drop();
    world.handoffs.tabRemoved(TAB);
    await flushPromises();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_left", reason: "closed" });
    world.updateTab({ title: "Пока страницы нет" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS);
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    // Новая вкладка грузится: её заголовки до `page_returned` плееру не положены.
    world.updateTab({ tabId: REOPENED_TAB, title: "Новая вкладка грузится" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS);
    assert.deepEqual(sent(world, "page_title_changed"), []);
    const returnedFilm: VideoCandidate = { ...PLAYING_VIDEO, document: "doc-new", element: 9 };
    assert.equal(world.handoffs.heldVideo(REOPENED_TAB, 0, { kind: HELD_VIDEO_REPORT_KIND, element: 9, candidates: [returnedFilm], trigger: "next_video" }), "adopt");
    // Сеанс уже переезжает (`moving`), но `page_returned` ещё не ушло: для плеера страница всё ещё ушла.
    world.updateTab({ tabId: REOPENED_TAB, title: "Тень ещё встаёт" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS);
    assert.deepEqual(sent(world, "page_title_changed"), []);
    const port = world.framePorts.last();
    const begin = port.posted.find((command) => command.kind === "begin_handoff");
    assert.ok(begin?.kind === "begin_handoff");
    port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    await flushPromises();
    world.updateTab({ tabId: REOPENED_TAB, title: "Серія 3 знову" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS);
    assert.deepEqual(sent(world, "page_title_changed"), [{ type: "page_title_changed", title: "Серія 3 знову" }]);
  });
});

describe("уход одностраничника: продуктовый мир и переезд посреди ролика (16a, решение 42)", () => {
  /** Реклама перед фильмом: файл с неизвестного домена (форма Ashdi и Tortuga), как в video-relocation.test.ts. */
  const AD: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 3, media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", playing: true, visible_area_px: 296_000, duration_s: 24 };
  const FILM: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 296_000, duration_s: 6381 };

  it("браузер знает окно страницы (её можно вернуть) — уход одностраничника всё равно tab_gone, а не сирота page_left", async () => {
    const reopen = createFakePageReopenBrowser({ url: SERIES_URL, windowId: BROWSER_WINDOW, incognito: false });
    const world = setup(undefined, undefined, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser });
    await startHandoff(world);
    deliverStream(world);
    world.updateTab({ url: "https://www.example.com/" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    await flushPromises();
    const socket = world.sockets.last();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.equal(socket.sentMessages().some((message) => message.type === "page_left"), false);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("реклама убрана, фильма нет 15 с, адрес сменился — уход, а не 4009", async () => {
    const world = setup([{ frameId: 0, result: [FILM, AD] }]);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.updateTab({ url: "https://www.example.com/" });
    world.clock.advance(NEXT_VIDEO_WAIT_MS);
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.deepEqual(sent(world, "awaiting_next_episode"), [], "конец рекламы — не конец серии");
  });

  it("реклама доиграла и осталась на странице, адрес сменился: сперва тень снимается, потом tab_gone", async () => {
    const world = setup([{ frameId: 0, result: [FILM, AD] }]);
    await startHandoff(world);
    clock(world, "ended", 1);
    world.updateTab({ url: "https://www.example.com/" });
    const port = world.framePorts.last();
    // Тень рекламы жива и подаёт пульс: сеанс кончает предел ожидания (15 с у рекламы), а не тишина.
    for (let waitedMs = 0; waitedMs < NEXT_VIDEO_WAIT_MS; waitedMs += SHADOW_SILENCE_LIMIT_MS / 2) {
      port.deliver({ kind: "shadow_alive" });
      world.clock.advance(SHADOW_SILENCE_LIMIT_MS / 2);
    }
    assert.deepEqual(port.posted.at(-1), { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("смена адреса и заголовка без потери источника сеанс не трогает: жив, не закрыт, заголовок ждёт нового видео (16a3)", async () => {
    const world = await series();
    const before = world.sockets.last().wireSummary();
    world.updateTab({ url: "https://www.example.com/watch?v=1&list=7", title: "Серія 3 (плейлист)" });
    world.updateTab({ url: "https://www.example.com/" });
    world.clock.advance(PAGE_TITLE_SETTLE_MS + PAGE_TITLE_AWAY_HOLD_MS - 1);
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.deepEqual(world.sockets.last().wireSummary().slice(before.length), [], "адрес ушёл с видео: заголовок — не его");
    assert.equal(world.framePorts.last().disconnectedByBackground, false, "порт к странице не тронут");
  });
});
