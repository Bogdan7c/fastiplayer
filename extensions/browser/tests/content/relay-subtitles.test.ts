// Изолированный мир и субтитры страницы (13b2): точные байты перед postMessage, своё происхождение
// документа в `subtitle_track_source`, бюджет вида `subtitle`, усыновление события из чужого отсека (Firefox).
import assert from "node:assert/strict";
import { runInNewContext } from "node:vm";
import { describe, it } from "node:test";
import { adoptPageValue } from "../../src/content/page-channel.ts";
import { PAGE_EVENT_LIMITS_PER_SECOND } from "../../src/content/page-event-budget.ts";
import { createContentRelay, type BackgroundPort } from "../../src/content/relay.ts";
import { createFakeClock } from "../background/support.ts";

class FakeBackgroundPort implements BackgroundPort {
  readonly received: { kind: string; [field: string]: unknown }[] = [];
  private readonly messageListeners: ((message: unknown) => void)[] = [];
  readonly onMessage = { addListener: (listener: (message: unknown) => void) => void this.messageListeners.push(listener) };
  readonly onDisconnect = { addListener: (_listener: () => void) => {} };
  postMessage(message: unknown): void {
    this.received.push(message as { kind: string });
  }
  disconnect(): void {}
  /** Фон прислал команду. */
  command(message: unknown): void {
    for (const listener of this.messageListeners) listener(message);
  }
}

const ownDocument = { hostname: "site.example", origin: "https://site.example" };
const NAMING = { language: "en", label: null, auto_generated: false };

function setupRelay(budget?: { limitsPerSecond: Record<string, number>; replayEventCredit: number }) {
  const clock = createFakeClock();
  const relay = createContentRelay({
    sendToPage: () => {},
    reportHeldVideo: () => Promise.resolve("adopt"),
    sendViewerOfferChoice: () => Promise.reject(new Error("not used")),
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: () => {},
    now: () => clock.scheduler.now(),
    ownDocument: () => ownDocument,
    ...(budget === undefined ? {} : { budgetLimits: budget as never }),
  });
  const port = new FakeBackgroundPort();
  relay.acceptBackgroundPort(port);
  return { relay, port };
}

/** Фон начал передачу `requestId`, MAIN-мир ответил «начал». */
function startHandoff(relay: ReturnType<typeof setupRelay>["relay"], port: FakeBackgroundPort, requestId = 1): void {
  port.command({ kind: "begin_handoff", request_id: requestId, document: "doc-a", element: 3 });
  relay.handlePageMessage({ kind: "handoff_began", request_id: requestId, outcome: "started" });
}

const documentEvent = (bytes: Uint8Array, overrides: Record<string, unknown> = {}) => ({
  kind: "subtitle_document",
  key: 1,
  naming: NAMING,
  format_hint: "web_vtt",
  origin: "page_download",
  display: "showing",
  bytes,
  ...overrides,
});

const trackSourceEvent = (overrides: Record<string, unknown> = {}) => ({
  kind: "subtitle_track_source",
  key: 2,
  naming: NAMING,
  display: "not_showing",
  url: "https://cdn.example/subs/en.vtt",
  referrer_origin: "https://bank.example",
  referrer_policy: "origin",
  ...overrides,
});

describe("subtitle_document на границе изолированного мира", () => {
  it("вид вместо точной копии режется перед отправкой фону: в порт уходит буфер ровно по размеру документа", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    const hugeBuffer = new ArrayBuffer(1024 * 1024);
    const view = new Uint8Array(hugeBuffer, 100, 6);
    view.set([87, 69, 66, 86, 84, 84]);
    relay.handlePageMessage(documentEvent(view));
    const sent = port.received.find((message) => message.kind === "subtitle_document");
    assert.ok(sent !== undefined);
    const bytes = sent["bytes"] as Uint8Array;
    assert.deepEqual([...bytes], [87, 69, 66, 86, 84, 84]);
    assert.equal(bytes.byteOffset, 0);
    assert.equal(bytes.buffer.byteLength, 6, "фону уходит не весь буфер страницы");
  });

  it("точная копия уходит как есть, прочие поля не тронуты", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    const bytes = new Uint8Array([1, 2, 3]);
    relay.handlePageMessage(documentEvent(bytes, { key: 7, origin: "text_track_cues", display: "not_showing" }));
    const sent = port.received.find((message) => message.kind === "subtitle_document");
    assert.deepEqual({ ...sent, bytes: undefined }, { ...documentEvent(bytes, { key: 7, origin: "text_track_cues", display: "not_showing" }), bytes: undefined });
    assert.equal(sent?.["bytes"], bytes, "уже точная копия — не копируется второй раз");
  });

  it("мусорное событие не доходит до фона и считается отказом", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    relay.handlePageMessage(documentEvent(new Uint8Array(0)));
    relay.handlePageMessage(documentEvent(new Uint8Array([1]), { key: 0 }));
    relay.handlePageMessage({ ...documentEvent(new Uint8Array([1])), extra: true });
    assert.equal(port.received.filter((message) => message.kind === "subtitle_document").length, 0);
    assert.equal(relay.counters().rejectedPageMessages, 3);
  });

  it("без идущей передачи документ некому отдать", () => {
    const { relay, port } = setupRelay();
    relay.handlePageMessage(documentEvent(new Uint8Array([1])));
    assert.equal(port.received.length, 0);
    assert.equal(relay.counters().undeliverablePageEvents, 1);
  });

  it("событие из чужого отсека (Firefox) усыновляется копией и проходит с байтами", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    const foreign = runInNewContext(
      '({ kind: "subtitle_document", key: 1, naming: { language: "en", label: null, auto_generated: false }, format_hint: "web_vtt", origin: "page_download", display: "showing", bytes: new Uint8Array([9, 8, 7]) })',
    ) as unknown;
    relay.handlePageMessage(foreign);
    assert.equal(port.received.filter((message) => message.kind === "subtitle_document").length, 0, "без усыновления чужая Uint8Array не проходит instanceof");
    relay.handlePageMessage(adoptPageValue(foreign));
    const sent = port.received.find((message) => message.kind === "subtitle_document");
    assert.deepEqual([...((sent?.["bytes"] as Uint8Array | undefined) ?? [])], [9, 8, 7]);
  });
});

describe("subtitle_track_source на границе изолированного мира", () => {
  it("происхождение для Referer берётся своё, а не страницы; null остаётся null", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    relay.handlePageMessage(trackSourceEvent());
    relay.handlePageMessage(trackSourceEvent({ key: 3, referrer_origin: null, referrer_policy: "no-referrer" }));
    const sent = port.received.filter((message) => message.kind === "subtitle_track_source");
    assert.equal(sent[0]?.["referrer_origin"], ownDocument.origin, "подмена страницы https://bank.example заменена своим");
    assert.equal(sent[1]?.["referrer_origin"], null);
    assert.equal(sent[0]?.["url"], "https://cdn.example/subs/en.vtt");
  });
});

describe("бюджет вида subtitle", () => {
  const TINY = {
    limitsPerSecond: { ...PAGE_EVENT_LIMITS_PER_SECOND, subtitle: 2 },
    replayEventCredit: 0,
  };

  it("документы и <track> делят одно ведро; сверх лимита — донесение page_flood «subtitle», пересылка стоит", () => {
    const { relay, port } = setupRelay(TINY);
    startHandoff(relay, port);
    relay.handlePageMessage(documentEvent(new Uint8Array([1]), { key: 1 }));
    relay.handlePageMessage(trackSourceEvent({ key: 2 }));
    relay.handlePageMessage(documentEvent(new Uint8Array([2]), { key: 3 }));
    relay.handlePageMessage(trackSourceEvent({ key: 4 }));
    const crossed = port.received.filter((message) => message.kind === "subtitle_document" || message.kind === "subtitle_track_source");
    assert.equal(crossed.length, 2, "дошёл только лимит");
    assert.deepEqual(port.received.at(-1), { kind: "page_flood", over_budget: "subtitle" });
    assert.equal(relay.counters().droppedFloodEvents, 2);
  });

  it("не тратит ведро mse: потоку видео субтитры не мешают", () => {
    const { relay, port } = setupRelay(TINY);
    startHandoff(relay, port);
    relay.handlePageMessage(documentEvent(new Uint8Array([1])));
    relay.handlePageMessage({ kind: "page_seek", target_us: 1 });
    assert.ok(port.received.some((message) => message.kind === "page_seek"));
    assert.equal(port.received.some((message) => message.kind === "page_flood"), false);
  });
});

describe("боевой лимит вида subtitle", () => {
  it("пачка начала передачи (32 документа и 128 дорожек за один проход) проходит целиком, без флуда", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    for (let index = 1; index <= 32; index += 1) relay.handlePageMessage(documentEvent(new Uint8Array([index]), { key: index }));
    for (let index = 1; index <= 128; index += 1) relay.handlePageMessage(trackSourceEvent({ key: 100 + index }));
    assert.equal(port.received.filter((message) => message.kind === "subtitle_document" || message.kind === "subtitle_track_source").length, 160);
    assert.equal(port.received.some((message) => message.kind === "page_flood"), false);
  });

  it("флуд «субтитрами» упирается в лимит: за секунду проходит не больше боевого лимита", () => {
    const { relay, port } = setupRelay();
    startHandoff(relay, port);
    for (let index = 1; index <= 1000; index += 1) relay.handlePageMessage(trackSourceEvent({ key: index }));
    assert.equal(port.received.filter((message) => message.kind === "subtitle_track_source").length, PAGE_EVENT_LIMITS_PER_SECOND.subtitle);
    assert.deepEqual(port.received.at(-1), { kind: "page_flood", over_budget: "subtitle" });
  });
});
