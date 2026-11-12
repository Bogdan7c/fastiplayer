// Перезагрузка страницы в той же вкладке (сессия 16c, решения 59, 304, 305, 215) на фоне целиком: `createTabHandoffs`
// с фейковыми часами, плеером, сокетами, портами и браузером. Проверяется то, что увидят плеер и страница: нет
// преждевременного конца, видео нового документа уходит в то же окно заменой, срок и прежние концы.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { NEXT_VIDEO_WAIT_MS } from "../../src/background/handoff/video-relocation.ts";
import { PROTOCOL_VERSION_1_3, SESSION_CLOSE_CODES, type PageSessionMessage, type ProtocolVersion } from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, SHADOW_SILENCE_LIMIT_MS, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../src/shared/next-episode-wait.ts";
import type { RangeFetch } from "../../src/background/handoff/range-fetcher.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { deliverStream, PLAYING_VIDEO, setup, startHandoff, TAB, type World, type WorldOptions } from "./handoff-world.ts";
import { flushPromises, grantFor, type FakeWebSocket } from "./support.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser } from "./page-return/fake-page-reopen.ts";

/** Адрес вкладки при старте (мир по умолчанию). */
const SERIES_URL = "https://www.example.com/watch?v=1";
/** Следующая серия: тот же основной домен, другой поддомен и путь (решение 304). */
const NEXT_EPISODE_URL = "https://example.com/watch?v=2";
const OTHER_SITE_URL = "https://other-site.test/";
/** Фильм нового документа после перезагрузки. */
const RELOADED_FILM: VideoCandidate = { ...PLAYING_VIDEO, document: "doc-reloaded", element: 3 };

const heldReport = (candidate: VideoCandidate): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger: "next_video" });

function clock(socket: FakeWebSocket, seq: number, state: "playing" | "ended"): void {
  socket.receive({ type: "clock", seq, position_us: seq * 1_000_000, state, rate: 1000 });
}

/** Типы сообщений плееру после отметки `fromIndex` провода (что ушло с момента перезагрузки). */
function wireSince(socket: FakeWebSocket, fromIndex: number): string[] {
  return socket.wireSummary().slice(fromIndex);
}

const sent = (socket: FakeWebSocket, type: PageSessionMessage["type"]): PageSessionMessage[] => socket.sentMessages().filter((message) => message.type === type);

interface Playing {
  readonly world: World;
  readonly socket: FakeWebSocket;
}

/** Серия играет в окне плеера: тень стоит, поколение отдано, часы идут. */
async function playing(options: WorldOptions & { readonly protocol?: ProtocolVersion; readonly frames?: Parameters<typeof setup>[0] } = {}): Promise<Playing> {
  const world = setup(options.frames, undefined, undefined, options);
  const protocol = options.protocol;
  if (protocol !== undefined) world.door.nextOutcome = (request) => grantFor(request, "started", protocol);
  await startHandoff(world);
  deliverStream(world);
  const socket = world.sockets.last();
  clock(socket, 1, "playing");
  return { world, socket };
}

/** Адрес вкладки не меняется: полная перезагрузка той же страницы. */
const SAME_ADDRESS = "same_address";

/** Документ ушёл: браузер сменил (или нет) адрес вкладки, порт документа закрылся. */
async function documentGone(world: World, address: string = NEXT_EPISODE_URL): Promise<void> {
  if (address !== SAME_ADDRESS) world.updateTab({ url: address });
  world.framePorts.last().drop();
  await flushPromises();
}

/** Новый документ показал фильм: опрос замены его найдёт. */
function newDocumentShows(world: World, candidate: VideoCandidate = RELOADED_FILM, frameId = 0): void {
  world.setFrames([{ frameId, result: [candidate] }]);
}

describe("документ ушёл, вкладка на том же сайте — окно ждёт видео нового документа", () => {
  it("(a) ни конца, ни паузы плееру, сторож взведён; видео нового документа — замена в том же окне", async () => {
    const { world, socket } = await playing();
    const armingsBefore = world.armings.length;
    const wireMark = socket.wireSummary().length;
    await documentGone(world);
    world.clock.advance(2_000);
    assert.deepEqual(world.ended, [], "сеанс жив");
    assert.deepEqual(wireSince(socket, wireMark), [], "ни tab_gone, ни page_left, ни паузы: окно досматривает скачанное");
    assert.ok(world.armings.length > armingsBefore, "сторож взведён во всех фреймах и продлевается");

    // Новый документ показал фильм, сторож его задержал: фон отвечает «новый сеанс» и открывает замену.
    newDocumentShows(world);
    world.door.nextOutcome = (request) => grantFor(request, "replaced_previous");
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport(RELOADED_FILM)), "adopt_new_session");
    await flushPromises();
    assert.equal(world.door.requests.length, 2, "плееру ушёл второй open_session — замена");
    const newSocket = world.sockets.sockets[1];
    assert.ok(newSocket !== undefined);
    newSocket.open();
    newSocket.receive({ type: "attached" });
    await flushPromises();
    const portsBeforeOldEnd = world.framePorts.ports.length;
    // Плеер заменил окно: старому сокету — прощание; старый сеанс кончается без команд странице (её нет).
    socket.receive({ type: "return_to_page", position_us: 9_000_000, resume: "paused" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.deepEqual(sent(socket, "tab_gone"), [], "старому плееру не врали «вкладка ушла»");
    const newPort = world.framePorts.last();
    assert.equal(world.framePorts.ports.length, portsBeforeOldEnd + 1, "страница замены — только после конца старого сеанса");
    const begin = newPort.posted.find((command): command is Extract<PageCommand, { kind: "begin_handoff" }> => command.kind === "begin_handoff");
    assert.ok(begin !== undefined);
    assert.deepEqual({ document: begin.document, element: begin.element }, { document: RELOADED_FILM.document, element: RELOADED_FILM.element });
    newPort.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    await flushPromises();
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "in_player", video: { frame_id: 0, document: RELOADED_FILM.document, element: RELOADED_FILM.element } });
    assert.deepEqual(world.releases, [], "видео взято, отпускать нечего");
  });

  it("(b) другой сайт уже в миг ухода — прежний конец сразу, без ожидания", async () => {
    const { world, socket } = await playing();
    await documentGone(world, OTHER_SITE_URL);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.deepEqual(sent(socket, "tab_gone"), [{ type: "tab_gone", reason: "navigated_away" }]);
  });

  it("(c) во время ожидания вкладка ушла на другой сайт — конец сразу, не по сроку", async () => {
    const { world } = await playing();
    await documentGone(world);
    world.clock.advance(1_000);
    assert.deepEqual(world.ended, []);
    world.updateTab({ url: OTHER_SITE_URL });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    // Видео чужого сайта сеанс уже не берёт: сеанса нет, донесение отпускается.
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport(RELOADED_FILM)), "release");
  });

  it("(d) срок из настройки: 7 с — tab_gone{navigated_away} у плеера без возврата страницы, ровно на 7-й секунде", async () => {
    const { world, socket } = await playing({ pageReloadWait: { seconds: 7 } });
    await documentGone(world);
    world.clock.advance(6_999);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.deepEqual(sent(socket, "tab_gone"), [{ type: "tab_gone", reason: "navigated_away" }]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
  });

  it("(d) срок вышел, страницу можно вернуть (плеер 1.4+) — сирота `page_left{navigated_away}`, а старому плееру — tab_gone", async () => {
    const reopen = createFakePageReopenBrowser({ url: SERIES_URL, windowId: BROWSER_WINDOW, incognito: false });
    const modern = await playing({ tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser });
    await documentGone(modern.world);
    modern.world.clock.advance(10_000);
    assert.deepEqual(sent(modern.socket, "page_left"), [{ type: "page_left", reason: "navigated_away" }]);
    assert.equal(modern.socket.closedWithCode, undefined, "сирота держит связь");

    const old = await playing({ tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser, protocol: PROTOCOL_VERSION_1_3 });
    await documentGone(old.world);
    old.world.clock.advance(10_000);
    assert.deepEqual(sent(old.socket, "tab_gone"), [{ type: "tab_gone", reason: "navigated_away" }]);
  });

  it("(d) 25 с из паузы между роликами: короткий срок переезда (15 с) не перебивает срок перезагрузки", async () => {
    const ad: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 3, media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", playing: true, visible_area_px: 296_000, duration_s: 24 };
    const film: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 296_000, duration_s: 6381 };
    const world = setup([{ frameId: 0, result: [film, ad] }], undefined, undefined, { pageReloadWait: { seconds: 25 } });
    await startHandoff(world);
    clock(world.sockets.last(), 1, "ended");
    // Реклама кончилась, страница её убрала: сеанс между роликами, идёт 15-секундный срок переезда.
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(1_000);
    await documentGone(world);
    world.clock.advance(NEXT_VIDEO_WAIT_MS);
    assert.deepEqual(world.ended, [], "срок переезда снят: два срока наперегонки кончали бы сеанс раньше");
    world.clock.advance(25_000 - NEXT_VIDEO_WAIT_MS - 1);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("(e) настройка 0 — прежнее поведение: уход страницы сразу", async () => {
    const { world, socket } = await playing({ pageReloadWait: { seconds: 0 } });
    await documentGone(world);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.deepEqual(sent(socket, "tab_gone"), [{ type: "tab_gone", reason: "navigated_away" }]);
  });

  it("сбой чтения настройки — срок по умолчанию (10 с) и отметка для разработчика, не молча", async () => {
    const { world } = await playing({ pageReloadWait: new Error("storage is gone") });
    assert.ok(world.adMirror.notes.some((note) => note.includes("could not read the page reload wait setting")));
    await documentGone(world);
    world.clock.advance(9_999);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });
});

describe("серия уже доиграла, когда документ ушёл — как 16a", () => {
  it("(f) `awaiting_next_episode` и срок 60 с, а не 10; по сроку — конец 16a «следующая не началась»", async () => {
    const { world, socket } = await playing();
    clock(socket, 2, "ended");
    await documentGone(world, SAME_ADDRESS);
    assert.deepEqual(sent(socket, "awaiting_next_episode"), [{ type: "awaiting_next_episode", patience_ms: NEXT_EPISODE_WAIT_MS }]);
    world.clock.advance(NEXT_EPISODE_WAIT_MS - 1);
    assert.deepEqual(world.ended, [], "10 с настройки здесь не действуют");
    world.clock.advance(1);
    assert.deepEqual(world.ended, [{ kind: "next_episode_not_started" }]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.next_episode_not_started);
  });

  it("(f) плеер во фрейме, серия доиграла, сайт открыл страницу следующей серии — ждём и берём её видео", async () => {
    const frames = [{ frameId: 0, result: [] }, { frameId: 5, result: [PLAYING_VIDEO] }];
    const { world, socket } = await playing({ frames });
    clock(socket, 2, "ended");
    await documentGone(world, NEXT_EPISODE_URL);
    world.clock.advance(NEXT_VIDEO_WAIT_MS);
    assert.deepEqual(world.ended, [], "переход по адресу — не уход, пока тот же сайт");
    assert.deepEqual(sent(socket, "awaiting_next_episode").length, 1);
    newDocumentShows(world, RELOADED_FILM, 7);
    assert.equal(world.handoffs.heldVideo(TAB, 7, heldReport(RELOADED_FILM)), "adopt_new_session");
  });
});

describe("передача сеансу-замене", () => {
  /** Документ ушёл, через 4 с сторож задержал фильм, а опрос замены его уже не найдёт (новый документ убрал видео). */
  async function handoverThatWillBeRefused(world: World): Promise<void> {
    await documentGone(world);
    world.clock.advance(4_000);
    world.setFrames([{ frameId: 0, result: [] }]);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport(RELOADED_FILM)), "adopt_new_session");
  }

  it("(g) второе видео, пока замена открывается, — release; замена не состоялась — видео отпущено, ждём остаток срока", async () => {
    const { world } = await playing();
    await handoverThatWillBeRefused(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport({ ...RELOADED_FILM, element: 8 })), "release", "одна замена за раз");
    await flushPromises();
    assert.deepEqual(world.releases, [{ tabId: TAB, frameId: 0, element: RELOADED_FILM.element }], "обещанное видео отпущено во фрейме");
    world.clock.advance(5_999);
    assert.deepEqual(world.ended, [], "остаток срока — 6 с");
    world.clock.advance(1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("(g) срок вышел, пока замена открывалась: замену не обрывают, а её отказ кончает сеанс сразу", async () => {
    const { world } = await playing();
    await handoverThatWillBeRefused(world);
    world.clock.advance(10_000);
    assert.deepEqual(world.ended, [], "пока замена открывается, срок не срабатывает");
    await flushPromises();
    assert.deepEqual(world.releases.length, 1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }], "остатка нет — конец сразу");
  });
});

describe("замена после перезагрузки — только замена и с ролью от суда сторожа (решения 215, 314)", () => {
  it("зритель закрыл окно, пока замена готовилась, — второго окна нет, задержанное видео отпущено в браузер", async () => {
    const { world, socket } = await playing();
    await documentGone(world);
    newDocumentShows(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport(RELOADED_FILM)), "adopt_new_session");
    // Опрос замены ещё в пути, а зритель закрыл окно: старого сеанса больше нет.
    socket.receive({ type: "return_to_page", position_us: 1_000_000, resume: "paused" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.equal(world.door.requests.length, 1, "второй open_session не уходил: новое окно не открывается");
    assert.deepEqual(world.releases, [{ tabId: TAB, frameId: 0, element: RELOADED_FILM.element }], "видео играет в браузере");
    assert.deepEqual(world.handoffs.activeTabIds(), []);
  });

  it("реклама перед фильмом на новой странице — в замене это реклама: плашка рекламы, потом фильм переездом того же сеанса", async () => {
    const ad: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-reloaded", element: 6, media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", playing: true, visible_area_px: 296_000, duration_s: 24 };
    const film: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-reloaded", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 296_000, duration_s: 6381 };
    const { world } = await playing();
    await documentGone(world);
    world.setFrames([{ frameId: 0, result: [film, ad] }]);
    world.door.nextOutcome = (request) => grantFor(request, "replaced_previous");
    // Донесение сторожа несёт все видео фрейма: рядом с рекламой стоит фильм (форма Ashdi и Tortuga).
    assert.equal(world.handoffs.heldVideo(TAB, 0, { ...heldReport(ad), candidates: [film, ad] }), "adopt_new_session");
    await flushPromises();
    const newSocket = world.sockets.sockets[1];
    assert.ok(newSocket !== undefined);
    newSocket.open();
    newSocket.receive({ type: "attached" });
    world.sockets.sockets[0]?.receive({ type: "return_to_page", position_us: 9_000_000, resume: "paused" });
    await flushPromises();
    const adPort = world.framePorts.last();
    const adBegin = adPort.posted.find((command): command is Extract<PageCommand, { kind: "begin_handoff" }> => command.kind === "begin_handoff");
    assert.ok(adBegin !== undefined && adBegin.element === ad.element);
    adPort.deliver({ kind: "handoff_began", request_id: adBegin.request_id, outcome: "started" });
    await flushPromises();
    assert.equal(world.handoffs.isAdOnScreen(TAB), true, "реклама осталась рекламой: плашка и дублёр пропуска");
    // Реклама доиграла и страница её убрала: тот же сеанс ждёт фильм коротким сроком и переезжает на него.
    clock(newSocket, 1, "ended");
    adPort.deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(sent(newSocket, "awaiting_next_episode"), [], "конец рекламы — не конец серии");
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldReport({ ...film, playing: true })), "adopt", "фильм — переезд того же сеанса");
    assert.equal(world.door.requests.length, 2, "нового сеанса для фильма нет");
  });
});

describe("концы во время ожидания", () => {
  it("(h) вкладку закрыли — «Source tab closed»: tab_gone{closed}", async () => {
    const { world, socket } = await playing();
    await documentGone(world);
    world.closeTab();
    world.handoffs.tabRemoved(TAB);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "closed" }]);
    assert.deepEqual(sent(socket, "tab_gone"), [{ type: "tab_gone", reason: "closed" }]);
  });

  it("(i) зритель закрыл окно (прощание или закрытие сокета) — конец без команд странице", async () => {
    const farewell = await playing();
    await documentGone(farewell.world);
    const postsBefore = farewell.world.framePorts.ports.map((port) => port.posted.length);
    farewell.socket.receive({ type: "return_to_page", position_us: 5_000_000, resume: "paused" });
    assert.deepEqual(farewell.world.ended, [{ kind: "returned_to_page" }]);
    assert.deepEqual(farewell.world.framePorts.ports.map((port) => port.posted.length), postsBefore, "ушедшему документу ничего не слали");

    const closed = await playing();
    await documentGone(closed.world);
    closed.socket.remoteClose(SESSION_CLOSE_CODES.player_closed);
    assert.equal(closed.world.ended[0]?.kind, "player_ended");
    assert.equal(closed.world.door.demandHeld(), 0);
  });

  it("(j) рекламный фрейм умер, адрес тот же — прежний путь 09c2: пауза плееру и срок переезда 15 с", async () => {
    const ad: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 3, media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", playing: true, visible_area_px: 296_000, duration_s: 24 };
    const world = setup([{ frameId: 0, result: [] }, { frameId: 5, result: [ad] }], undefined, undefined, { pageReloadWait: { seconds: 30 } });
    await startHandoff(world);
    const socket = world.sockets.last();
    world.framePorts.last().drop();
    await flushPromises();
    assert.ok(socket.wireSummary().includes("page_playback"), "пауза до следующего ролика, как в 09c2");
    world.clock.advance(NEXT_VIDEO_WAIT_MS);
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }], "срок переезда, а не 30 с ожидания перезагрузки");
  });

  it("(k) пульса тени нет 30 с ожидания — сторож тишины молчит (конец только по сроку)", async () => {
    const { world } = await playing({ pageReloadWait: { seconds: 30 } });
    await documentGone(world);
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS * 2);
    assert.deepEqual(world.ended, [], "тишина мёртвого документа — не вина страницы");
    world.clock.advance(30_000 - SHADOW_SILENCE_LIMIT_MS * 2);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("плеер просит байты файла во время ожидания — фон их отдаёт: окно досматривает скачанное", async () => {
    const fileCandidate: VideoCandidate = { ...PLAYING_VIDEO, media: { kind: "file" }, file_host: "cdn.example" };
    const fileServer: RangeFetch = (_url, init) => {
      const match = /^bytes=(\d+)-(\d+)$/.exec(init.headers["Range"] ?? "");
      assert.ok(match);
      const first = Number(match[1]);
      const last = Number(match[2]);
      return Promise.resolve(new Response(new Uint8Array(last - first + 1), { status: 206, headers: { "content-range": `bytes ${first}-${last}/1000000` } }));
    };
    const world = setup([{ frameId: 0, result: [fileCandidate] }], fileServer);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: "https://cdn.example/movie.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    await documentGone(world);
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    for (let round = 0; round < 20; round += 1) await flushPromises();
    assert.deepEqual(socket.wireSummary().slice(-2), ["range 1@0+4", "range_fetch_finished"]);
    assert.deepEqual(world.ended, []);
  });

  it("команды странице теряются (порт мёртв, onDisconnect не пришёл), сайт тот же — один вопрос, ожидание, один конец по сроку", async () => {
    const { world, socket } = await playing();
    world.framePorts.last().failPostsWhere = () => true;
    for (let seq = 2; seq <= 5; seq += 1) clock(socket, seq, "playing");
    await flushPromises();
    assert.deepEqual(world.ended, [], "тот же сайт — ждём, а не кончаем сразу");
    world.clock.advance(10_000);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.equal(socket.wireSummary().filter((kind) => kind === "tab_gone").length, 1);
  });
});
