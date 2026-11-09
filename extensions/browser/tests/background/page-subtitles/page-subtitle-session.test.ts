// Субтитры страницы через весь фон (сессия 13b2): события страницы → то, что увидит сторож плеера на проводе
// (`subtitle_track_offered` и рамки вида 3), на фейковых часах, сокете, портах и сети.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES, MAX_PAGE_SUBTITLE_TRACKS } from "../../../src/protocol/limits.ts";
import { PROTOCOL_VERSION_1_4 } from "../../../src/protocol/version.ts";
import { SESSION_CLOSE_CODES } from "../../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type VideoCandidate } from "../../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../../support/video-candidate-defaults.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser } from "../page-return/fake-page-reopen.ts";
import { PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "../handoff-world.ts";
import { flushPromises, grantFor, type FakeFramePort, type FakeWebSocket } from "../support.ts";
import {
  bytesOf,
  documentEvent,
  hangingBody,
  networkResponse,
  recordingNetwork,
  subtitleDocumentsOnWire,
  TRACK_URL,
  trackSourceEvent,
  VTT_TEXT,
} from "./subtitle-support.ts";

async function settle(): Promise<void> {
  for (let round = 0; round < 20; round += 1) await flushPromises();
}

/** Мир с сетью, ответ которой задаёт сценарий; сеть видит мир (для проверки правил во время запроса). */
async function worldWithNetwork(answer: Parameters<typeof recordingNetwork>[0], options: { protocol?: typeof PROTOCOL_VERSION_1_4 } = {}) {
  const network = recordingNetwork(answer);
  const world = setup(undefined, network.fetch);
  const protocol = options.protocol;
  if (protocol !== undefined) world.door.nextOutcome = (request) => grantFor(request, "started", protocol);
  await startHandoff(world);
  return { world, network, port: world.framePorts.last(), socket: world.sockets.last() };
}

const textNotes = (world: World): string => world.adMirror.notes.join("\n");

describe("документ страницы → провод (протокол 1.5)", () => {
  it("subtitle_document → ровно один subtitle_track_offered с верными полями и рамки вида 3 с байтами документа", async () => {
    const { port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")));
    const before = socket.sent.length;
    port.deliver(documentEvent({ key: 7, naming: { language: "uk", label: "Українська", auto_generated: false }, format_hint: "web_vtt", origin: "text_track_cues", display: "not_showing" }));
    assert.equal(socket.sent.length - before, 2, "заголовок и одна рамка");
    const [sent, ...rest] = subtitleDocumentsOnWire(socket);
    assert.equal(rest.length, 0);
    assert.deepEqual(sent?.offer, {
      type: "subtitle_track_offered",
      track: 1,
      language: { kind: "known", tag: "uk" },
      label: { kind: "text", text: "Українська" },
      format_hint: "web_vtt",
      origin: "text_track_cues",
      page_display: "not_showing",
      byte_length: bytesOf(VTT_TEXT).length,
    });
    assert.equal(new TextDecoder().decode(sent?.bytes), VTT_TEXT);
    assert.equal(socket.closedWithCode, undefined, "сеанс жив");
  });

  it("длинный документ режется на несколько рамок подряд, байты сходятся", async () => {
    const { port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")));
    const big = new Uint8Array(2_500_000).map((_, index) => index % 251);
    port.deliver(documentEvent({ bytes: big }));
    const [sent] = subtitleDocumentsOnWire(socket);
    assert.ok(socket.sent.length > 3, "несколько рамок");
    assert.deepEqual(sent?.bytes, big);
  });

  it("имя приведено к протоколу: неверный тег — unknown, длинная подпись обрезана по символу, автосубтитры названы по языку", async () => {
    const { port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")));
    port.deliver(documentEvent({ key: 1, naming: { language: "en_US", label: "字".repeat(200), auto_generated: false } }));
    port.deliver(documentEvent({ key: 2, naming: { language: "en", label: null, auto_generated: true } }));
    const [first, second] = subtitleDocumentsOnWire(socket);
    assert.deepEqual(first?.offer.language, { kind: "unknown" });
    assert.deepEqual(first?.offer.label, { kind: "text", text: "字".repeat(85) });
    assert.deepEqual(second?.offer.label, { kind: "text", text: "English (auto-generated)" });
  });

  it("на версии 1.4 — ничего: ни сообщения, ни рамки, ни закачки; сеанс живёт", async () => {
    const { world, network, port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")), { protocol: PROTOCOL_VERSION_1_4 });
    const before = [...socket.sent];
    port.deliver(documentEvent());
    port.deliver(trackSourceEvent());
    await settle();
    assert.deepEqual(socket.sent, before, "провод не изменился");
    assert.equal(network.calls.length, 0, "плееру 1.4 субтитры не нужны — в сеть не ходим");
    assert.equal(socket.closedWithCode, undefined);
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB]);
    assert.match(textNotes(world), /player_too_old/);
  });
});

describe("номера дорожек на сеанс", () => {
  it("два документа одного фрейма — разные номера; тот же ключ — тот же номер (замена)", async () => {
    const { port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")));
    port.deliver(documentEvent({ key: 1 }));
    port.deliver(documentEvent({ key: 2 }));
    port.deliver(documentEvent({ key: 1, bytes: bytesOf("WEBVTT\n\n00:00.000 --> 00:02.000\nLonger\n") }));
    const documents = subtitleDocumentsOnWire(socket);
    assert.deepEqual(documents.map((document) => document.offer.track), [1, 2, 1]);
    assert.equal(new TextDecoder().decode(documents[2]?.bytes).includes("Longer"), true);
  });

  /**
   * Сеанс на рекламе (`doc-ad`, фрейм 0), ключ 1 отправлен; страница отпускает рекламу, и сторож
   * приносит фильм из фрейма `filmFrame`: фрейм 0 — тот же документ и тот же порт, другой фрейм —
   * другой документ и новый порт.
   */
  async function movedToFilm(filmFrame: number): Promise<{ readonly world: World; readonly filmPort: FakeFramePort }> {
    const film: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: filmFrame === 0 ? "doc-ad" : "doc-film", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 296_000, duration_s: 6381 };
    const ad: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 3, media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", playing: true, visible_area_px: 296_000, duration_s: 24 };
    const world = setup([{ frameId: 0, result: [film, ad] }]);
    await startHandoff(world);
    const adPort = world.framePorts.last();
    adPort.deliver(documentEvent({ key: 1 }));
    adPort.deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(world.handoffs.heldVideo(TAB, filmFrame, { kind: HELD_VIDEO_REPORT_KIND, element: film.element, candidates: [{ ...film, playing: true }], trigger: "next_video" }), "adopt");
    const filmPort = world.framePorts.last();
    assert.equal(filmPort === adPort, filmFrame === 0, "тот же фрейм — тот же порт, другой — новый");
    const begin = filmPort.posted.filter((command) => command.kind === "begin_handoff").at(-1);
    assert.ok(begin?.kind === "begin_handoff");
    filmPort.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    return { world, filmPort };
  }

  it("второе видео ТОГО ЖЕ документа: склад страницы тот же, ключ 1 заменяет ту же дорожку (без дубля в меню)", async () => {
    const { world, filmPort } = await movedToFilm(0);
    filmPort.deliver(documentEvent({ key: 1 }));
    filmPort.deliver(documentEvent({ key: 2 }));
    assert.deepEqual(subtitleDocumentsOnWire(world.sockets.last()).map((document) => document.offer.track), [1, 1, 2]);
  });

  it("сеанс переехал на ДРУГОЙ документ: те же ключи получают новые номера, старые не повторяются", async () => {
    const { world, filmPort } = await movedToFilm(1);
    filmPort.deliver(documentEvent({ key: 1 }));
    assert.deepEqual(subtitleDocumentsOnWire(world.sockets.last()).map((document) => document.offer.track), [1, 2]);
  });

  it(`${MAX_PAGE_SUBTITLE_TRACKS + 1}-я дорожка — отказ (в журнале), первые ${MAX_PAGE_SUBTITLE_TRACKS} живы, сеанс не закрыт`, async () => {
    const { world, port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")));
    for (let key = 1; key <= MAX_PAGE_SUBTITLE_TRACKS + 1; key += 1) port.deliver(documentEvent({ key }));
    const documents = subtitleDocumentsOnWire(socket);
    assert.equal(documents.length, MAX_PAGE_SUBTITLE_TRACKS);
    assert.deepEqual(documents.map((document) => document.offer.track), Array.from({ length: MAX_PAGE_SUBTITLE_TRACKS }, (_, index) => index + 1));
    assert.match(textNotes(world), /too_many_tracks/);
    assert.equal(socket.closedWithCode, undefined);
    // Живая дорожка продолжает заменяться.
    port.deliver(documentEvent({ key: 3 }));
    assert.equal(subtitleDocumentsOnWire(socket).length, MAX_PAGE_SUBTITLE_TRACKS + 1);
  });

  it("бюджет 32 МиБ: пятый крупный документ отказан, замена меньшим освобождает место", async () => {
    const { world, port, socket } = await worldWithNetwork(() => Promise.reject(new Error("сеть не нужна")));
    const full = new Uint8Array(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES);
    for (let key = 1; key <= 4; key += 1) port.deliver(documentEvent({ key, bytes: full }));
    port.deliver(documentEvent({ key: 5, bytes: bytesOf(VTT_TEXT) }));
    assert.deepEqual(subtitleDocumentsOnWire(socket).map((document) => document.offer.track), [1, 2, 3, 4]);
    assert.match(textNotes(world), /over_budget/);
    port.deliver(documentEvent({ key: 1, bytes: bytesOf(VTT_TEXT) }));
    port.deliver(documentEvent({ key: 5, bytes: bytesOf(VTT_TEXT) }));
    assert.deepEqual(subtitleDocumentsOnWire(socket).map((document) => document.offer.track), [1, 2, 3, 4, 1, 5]);
  });
});

describe("<track src> скачивает фон", () => {
  it("успех: закачка с куками → документ плееру с origin track_element и показом из события; адрес в провод не попал", async () => {
    const { network, port, socket } = await worldWithNetwork(() => Promise.resolve(networkResponse(bytesOf(VTT_TEXT))));
    port.deliver(trackSourceEvent({ display: "showing" }));
    await settle();
    assert.equal(network.calls[0]?.url, TRACK_URL);
    assert.equal(network.calls[0]?.init.credentials, "include");
    const [sent] = subtitleDocumentsOnWire(socket);
    assert.equal(sent?.offer.origin, "track_element");
    assert.equal(sent?.offer.page_display, "showing");
    assert.equal(sent?.offer.format_hint, "web_vtt");
    assert.equal(new TextDecoder().decode(sent?.bytes), VTT_TEXT);
    assert.ok(!socket.sent.some((payload) => typeof payload === "string" && payload.includes("cdn.example")), "адреса плееру нет");
  });

  it("Referer: правило стоит ВО ВРЕМЯ запроса со значением по политике страницы и снято после", async () => {
    let world: World | undefined;
    let ruleCountDuringRequest = -1;
    let refererDuringRequest: string | undefined;
    const holder = await worldWithNetwork(() => {
      assert.ok(world !== undefined);
      ruleCountDuringRequest = world.sessionRules.requestRuleCount();
      const rule = world.sessionRules.addedRequestRules().at(-1);
      refererDuringRequest = rule?.action.type === "modifyHeaders" ? rule.action.requestHeaders?.[0]?.value : undefined;
      return Promise.resolve(networkResponse(bytesOf(VTT_TEXT)));
    });
    world = holder.world;
    holder.port.deliver(trackSourceEvent({ referrer_origin: "https://www.example.com", referrer_policy: "origin" }));
    await settle();
    assert.equal(ruleCountDuringRequest, 1);
    assert.equal(refererDuringRequest, "https://www.example.com/");
    assert.equal(world.sessionRules.requestRuleCount(), 0, "после запроса правило снято");
    assert.equal(subtitleDocumentsOnWire(holder.socket).length, 1);
  });

  it("без Referer по политике (no-referrer) правило не ставится", async () => {
    const { world, port } = await worldWithNetwork(() => Promise.resolve(networkResponse(bytesOf(VTT_TEXT))));
    port.deliver(trackSourceEvent({ referrer_origin: "https://www.example.com", referrer_policy: "no-referrer" }));
    await settle();
    assert.equal(world.sessionRules.addedRequestRules().length, 0);
  });

  it("локальный адрес со страницы из интернета: заслон отвергает — запрос не ушёл, правила нет, плееру ничего", async () => {
    const { world, network, port, socket } = await worldWithNetwork(() => Promise.reject(new Error("не должно быть запроса")));
    const before = [...socket.sent];
    port.deliver(trackSourceEvent({ url: "http://192.168.1.5/subs.vtt", referrer_origin: "https://www.example.com", referrer_policy: "origin" }));
    await settle();
    assert.equal(network.calls.length, 0);
    assert.equal(world.sessionRules.addedRequestRules().length, 0);
    assert.deepEqual(socket.sent, before);
    assert.match(textNotes(world), /address_refused/);
  });

  it("редирект на локальный адрес: тело отброшено, плееру ничего", async () => {
    const { world, port, socket } = await worldWithNetwork(() => Promise.resolve(networkResponse(bytesOf(VTT_TEXT), { url: "http://127.0.0.1/steal.vtt" })));
    port.deliver(trackSourceEvent());
    await settle();
    assert.equal(subtitleDocumentsOnWire(socket).length, 0);
    assert.match(textNotes(world), /address_refused/);
  });

  it("тело больше 8 МиБ по Content-Length: не читалось, плееру ничего", async () => {
    let pulls = 0;
    const body = new ReadableStream<Uint8Array>({ pull: (controller) => void (pulls += 1, controller.enqueue(new Uint8Array(10))) });
    const { world, port, socket } = await worldWithNetwork(() => Promise.resolve(networkResponse(body, { headers: { "content-length": String(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES + 1) } })));
    port.deliver(trackSourceEvent());
    await settle();
    assert.ok(pulls <= 1);
    assert.equal(subtitleDocumentsOnWire(socket).length, 0);
    assert.match(textNotes(world), /too_large/);
  });

  it("тело больше 8 МиБ по счёту байт (длины нет): отказ", async () => {
    const piece = new Uint8Array(1024 * 1024);
    const body = new ReadableStream<Uint8Array>({ pull: (controller) => controller.enqueue(piece) });
    const { world, port, socket } = await worldWithNetwork(() => Promise.resolve(networkResponse(body)));
    port.deliver(trackSourceEvent());
    await settle();
    assert.equal(subtitleDocumentsOnWire(socket).length, 0);
    assert.match(textNotes(world), /too_large/);
  });

  it("http 404 и не-субтитры: отказ с причиной в журнале, сеанс жив", async () => {
    let answerNotFound = true;
    const { world, port, socket } = await worldWithNetwork(() =>
      Promise.resolve(answerNotFound ? networkResponse(bytesOf("nope"), { status: 404 }) : networkResponse(bytesOf("<html>login</html>"))),
    );
    port.deliver(trackSourceEvent({ key: 1 }));
    await settle();
    assert.match(textNotes(world), /http_error 404/);
    answerNotFound = false;
    port.deliver(trackSourceEvent({ key: 2 }));
    await settle();
    assert.match(textNotes(world), /not_subtitles/);
    assert.equal(subtitleDocumentsOnWire(socket).length, 0);
    assert.equal(socket.closedWithCode, undefined);
  });

  it("кусочек HLS не становится дорожкой", async () => {
    const segment = "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:00.000 --> 00:01.000\nHi\n";
    const { world, port, socket } = await worldWithNetwork(() => Promise.resolve(networkResponse(bytesOf(segment))));
    port.deliver(trackSourceEvent());
    await settle();
    assert.equal(subtitleDocumentsOnWire(socket).length, 0);
    assert.match(textNotes(world), /hls_segment/);
  });
});

describe("после конца потока ничего не шлётся", () => {
  async function hangingTrack() {
    const world = await worldWithNetwork((_url, init) => Promise.resolve(networkResponse(hangingBody(init.signal))));
    world.port.deliver(trackSourceEvent());
    await settle();
    assert.equal(world.network.calls.length, 1, "закачка идёт");
    return world;
  }

  it("конец сеанса отменяет закачку; после неё провод не меняется", async () => {
    const { network, port, socket } = await hangingTrack();
    const before = [...socket.sent];
    port.deliver({ kind: "handoff_ended", cause: "element_released" });
    await settle();
    assert.equal(network.calls[0]?.init.signal.aborted, true);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(socket.sent, before);
  });

  it("плеер вернул страницу (фаза ending): готовая закачка плееру не отправляется", async () => {
    let finishDownload: () => void = () => {};
    const world = await worldWithNetwork(() => new Promise((resolve) => (finishDownload = () => resolve(networkResponse(bytesOf(VTT_TEXT))))));
    world.port.deliver(trackSourceEvent());
    await settle();
    world.socket.receive({ type: "return_to_page", position_us: 0, resume: "paused" });
    finishDownload();
    await settle();
    assert.equal(subtitleDocumentsOnWire(world.socket).length, 0);
    world.port.deliver(documentEvent());
    assert.equal(subtitleDocumentsOnWire(world.socket).length, 0, "и новые события тоже");
  });

  it("страница ушла (сирота, page_left): закачка отменена, к плееру после page_left ни байта", async () => {
    const reopen = createFakePageReopenBrowser({ url: "https://www.example.com/watch?v=1", windowId: BROWSER_WINDOW, incognito: false });
    const network = recordingNetwork((_url, init) => Promise.resolve(networkResponse(hangingBody(init.signal))));
    const world = setup([{ frameId: 0, result: [PLAYING_VIDEO] }], network.fetch, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser });
    await startHandoff(world);
    const port = world.framePorts.last();
    const socket: FakeWebSocket = world.sockets.last();
    port.deliver(trackSourceEvent());
    await settle();
    world.closeTab();
    port.drop();
    world.handoffs.tabRemoved(TAB);
    await settle();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "page_left", reason: "closed" });
    assert.equal(network.calls[0]?.init.signal.aborted, true);
    const after = socket.sent.length;
    port.deliver(documentEvent());
    await settle();
    assert.equal(socket.sent.length, after);
    assert.equal(socket.closedWithCode, undefined, "сторож плеера не получил повода закрыть 4003");
  });
});
