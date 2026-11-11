// Изолированный мир: рукопожатие с MAIN-миром, пограничник (отказ мусору), пересылка в обе стороны,
// выбор кандидатов с таймаутом, страховка тени при пропаже фона.
import type { HeldVideoReply, HeldVideoReport } from "../../src/shared/handoff-contract.ts";
import assert from "node:assert/strict";
import { runInNewContext } from "node:vm";
import { describe, it } from "node:test";
import { adoptPageValue, openPageChannel, type PageChannelPort } from "../../src/content/page-channel.ts";
import { createContentRelay, DESCRIBE_CANDIDATES_TIMEOUT_MS, exactBytes, HELD_VIDEO_REPLY_TIMEOUT_MS, HELD_VIDEO_REPORTS_PER_SECOND, type BackgroundPort } from "../../src/content/relay.ts";
import { JOURNAL_REPLAY_EVENT_CREDIT, PAGE_EVENT_LIMITS_PER_SECOND, type PageEventBudgetParameters } from "../../src/content/page-event-budget.ts";
import type { PageCommand } from "../../src/shared/handoff-contract.ts";
import { HANDSHAKE_MARKER_FIELD, HANDSHAKE_VERSION, PAGE_CHANNEL_READY_EVENT } from "../../src/shared/page-channel-handshake.ts";
import { parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock } from "../background/support.ts";

class FakeBackgroundPort implements BackgroundPort {
  readonly received: unknown[] = [];
  disconnected = false;
  private readonly messageListeners: ((message: unknown) => void)[] = [];
  private readonly disconnectListeners: (() => void)[] = [];
  readonly onMessage = { addListener: (listener: (message: unknown) => void) => void this.messageListeners.push(listener) };
  readonly onDisconnect = { addListener: (listener: () => void) => void this.disconnectListeners.push(listener) };
  postMessage(message: unknown): void {
    this.received.push(message);
  }
  disconnect(): void {
    this.disconnected = true;
  }
  /** Фон прислал команду. */
  command(message: unknown): void {
    for (const listener of this.messageListeners) listener(message);
  }
  /** Фон пропал (service worker упал, сеанс закрыт). */
  drop(): void {
    for (const listener of this.disconnectListeners) listener();
  }
}

/** Что о себе знает документ изолированного мира (не то, что наговорит страница). */
const ownDocument = { hostname: "site.example", origin: "https://site.example" };

function setupRelay(budgetLimits?: Omit<PageEventBudgetParameters, "now">) {
  const clock = createFakeClock();
  const toPage: unknown[] = [];
  const debugLines: string[] = [];
  const heldReports: HeldVideoReport[] = [];
  let heldReply: () => Promise<HeldVideoReply> = () => Promise.resolve("adopt");
  const relay = createContentRelay({
    sendToPage: (message) => void toPage.push(message),
    reportHeldVideo: (report) => {
      heldReports.push(report);
      return heldReply();
    },
    sendViewerOfferChoice: () => Promise.reject(new Error("the viewer offer is not used here")),
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: (line) => void debugLines.push(line),
    now: () => clock.scheduler.now(),
    ownDocument: () => ownDocument,
    ...(budgetLimits === undefined ? {} : { budgetLimits }),
  });
  return { clock, relay, toPage, debugLines, heldReports, answerHeldWith: (answer: () => Promise<HeldVideoReply>) => void (heldReply = answer) };
}

/** Фон начал передачу `requestId`, MAIN-мир ответил «начал». */
function beginStarted(relay: ReturnType<typeof setupRelay>["relay"], port: FakeBackgroundPort, requestId: number): void {
  port.command({ kind: "begin_handoff", request_id: requestId, document: "doc-a", element: 3 });
  relay.handlePageMessage({ kind: "handoff_began", request_id: requestId, outcome: "started" });
}

const END_AS_SHADOW: PageCommand = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } };

describe("рукопожатие участка 1", () => {
  it("настоящий MessageChannel: порт уходит MAIN-миру одним сообщением, обмен идёт по порту в обе стороны", async () => {
    const posted: { message: unknown; transfer: Transferable[] }[] = [];
    const fromPage: unknown[] = [];
    const ownWindow = new EventTarget();
    const channel = openPageChannel(
      {
        createChannel: () => {
          const messageChannel = new MessageChannel();
          return { port1: messageChannel.port1 as unknown as PageChannelPort, port2: messageChannel.port2 as unknown as Transferable };
        },
        postToOwnWindow: (message, transfer) => void posted.push({ message, transfer }),
        rendezvous: ownWindow,
        createCancelableEvent: (type) => new Event(type, { cancelable: true }),
      },
      (message) => fromPage.push(message),
    );
    assert.equal(posted.length, 0, "MAIN-мир ещё не звал: порт не уходит в пустоту");
    ownWindow.dispatchEvent(new Event(PAGE_CHANNEL_READY_EVENT));
    assert.equal(posted.length, 1);
    assert.deepEqual(posted[0]?.message, { [HANDSHAKE_MARKER_FIELD]: HANDSHAKE_VERSION });
    assert.equal(posted[0]?.transfer.length, 1, "ровно один порт");
    // «MAIN-мир»: берёт порт из рукопожатия и шлёт байты, отвечает на команду.
    const mainPort = posted[0]?.transfer[0] as unknown as MessagePort;
    const toMain: unknown[] = [];
    mainPort.onmessage = (event) => toMain.push(event.data);
    mainPort.postMessage({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([7, 8, 9]) });
    channel.send({ kind: "describe_candidates", request_id: 1 });
    await new Promise((resolve) => setTimeout(resolve, 20));
    try {
      assert.equal(fromPage.length, 1);
      const event = parsePageEvent(fromPage[0]);
      assert.ok(event?.kind === "bytes");
      assert.deepEqual([...event.bytes], [7, 8, 9]);
      assert.deepEqual(toMain, [{ kind: "describe_candidates", request_id: 1 }]);
    } finally {
      mainPort.close();
    }
  });

  it("объект из чужого отсека (как в Firefox) усыновляется копией и тогда проходит проверку", () => {
    const foreign = runInNewContext('({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([1, 2]) })') as unknown;
    assert.equal(parsePageEvent(foreign), undefined, "без усыновления чужой Uint8Array не проходит instanceof");
    const adopted = adoptPageValue(foreign);
    assert.ok(adopted instanceof Object);
    const event = parsePageEvent(adopted);
    assert.ok(event?.kind === "bytes");
    assert.deepEqual([...event.bytes], [1, 2]);
  });

  it("свой объект не копируется; неклонируемое из чужого отсека — мусор", () => {
    const own = { kind: "page_seek", target_us: 1 };
    assert.equal(adoptPageValue(own), own);
    const foreignFunction = runInNewContext("({ kind: () => 1 })") as unknown;
    assert.equal(adoptPageValue(foreignFunction), undefined);
  });
});

describe("пограничник", () => {
  it("до порта фона ничего не шлёт: события считаются неотданными, мусор — отвергнутым", () => {
    const { relay, debugLines } = setupRelay();
    relay.handlePageMessage({ kind: "page_seek", target_us: 1 });
    relay.handlePageMessage({ kind: "page_seek", target_us: "1" });
    relay.handlePageMessage("garbage");
    assert.deepEqual(relay.counters(), { rejectedPageMessages: 2, undeliverablePageEvents: 1, rejectedBackgroundMessages: 0, droppedFloodEvents: 0 });
    assert.equal(debugLines.length, 1, "одна строка диагностики за всё время");
  });

  it("договорные события передачи уходят фону, мусор — нет", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    relay.handlePageMessage({ kind: "page_seek", target_us: 5 });
    relay.handlePageMessage({ kind: "page_seek", target_us: 5, extra: 1 });
    relay.handlePageMessage({ kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" } });
    assert.deepEqual(port.received, [
      { kind: "handoff_began", request_id: 1, outcome: "started" },
      { kind: "page_seek", target_us: 5 },
      { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" } },
    ]);
    assert.equal(relay.counters().rejectedPageMessages, 1);
  });

  it("события вне передачи и ответ на чужой запрос фону не уходят", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    relay.handlePageMessage({ kind: "page_seek", target_us: 5 });
    relay.handlePageMessage({ kind: "handoff_began", request_id: 77, outcome: "started" });
    assert.deepEqual(port.received, []);
    assert.equal(relay.counters().undeliverablePageEvents, 2);
  });

  it("байты-вид на большой буфер уходят фону точной копией (иначе ушёл бы весь буфер)", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    const pageBuffer = new Uint8Array(1024).map((_, index) => index % 256);
    relay.handlePageMessage({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: pageBuffer.subarray(100, 110) });
    const sent = port.received[1] as { bytes: Uint8Array };
    assert.equal(sent.bytes.buffer.byteLength, 10);
    assert.deepEqual([...sent.bytes], [...pageBuffer.subarray(100, 110)]);
    const exact = new Uint8Array([1, 2]);
    assert.equal(exactBytes(exact), exact, "точный буфер не копируется");
  });

  it("команды фона уходят MAIN-миру; неизвестные — нет", () => {
    const { relay, toPage } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    port.command({ kind: "begin_handoff", request_id: 1, document: "doc-a", element: 3 });
    port.command({ kind: "shadow", command: { kind: "pause" } });
    port.command({ kind: "eval", code: "alert(1)" });
    assert.deepEqual(toPage, [
      { kind: "begin_handoff", request_id: 1, document: "doc-a", element: 3 },
      { kind: "shadow", command: { kind: "pause" } },
    ]);
    assert.equal(relay.counters().rejectedBackgroundMessages, 1);
  });

  it("фон пропал посреди передачи — тень снимается «на позиции тени, как тень»", () => {
    const { relay, toPage } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    port.command({ kind: "begin_handoff", request_id: 1, document: "doc-a", element: 3 });
    relay.handlePageMessage({ kind: "handoff_began", request_id: 1, outcome: "started" });
    port.drop();
    assert.deepEqual(toPage.at(-1), END_AS_SHADOW);
  });

  it("фон пропал, пока MAIN-мир начинал: начатая позже тень сразу снимается", () => {
    const { relay, toPage } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    port.command({ kind: "begin_handoff", request_id: 1, document: "doc-a", element: 3 });
    port.drop();
    assert.equal(toPage.length, 1, "пока тени нет, снимать нечего");
    relay.handlePageMessage({ kind: "handoff_began", request_id: 1, outcome: "started" });
    assert.deepEqual(toPage.at(-1), END_AS_SHADOW);
  });

  it("передача кончилась штатно — пропажа фона тень не трогает", () => {
    const { relay, toPage } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    port.command({ kind: "begin_handoff", request_id: 1, document: "doc-a", element: 3 });
    relay.handlePageMessage({ kind: "handoff_began", request_id: 1, outcome: "started" });
    relay.handlePageMessage({ kind: "handoff_ended", cause: "returned" });
    port.drop();
    assert.equal(toPage.length, 1);
  });

  it("новый порт фона заменяет старый: старый закрыт, активная тень снята, старый больше не командует", () => {
    const { relay, toPage } = setupRelay();
    const first = new FakeBackgroundPort();
    relay.acceptBackgroundPort(first);
    beginStarted(relay, first, 1);
    const second = new FakeBackgroundPort();
    relay.acceptBackgroundPort(second);
    assert.equal(first.disconnected, true);
    assert.deepEqual(toPage.at(-1), END_AS_SHADOW);
    first.command({ kind: "shadow", command: { kind: "play" } });
    assert.deepEqual(toPage.at(-1), END_AS_SHADOW, "старый порт больше не командует");
  });

  it("замена порта посреди передачи: хвост старой не попадает в новую, а новая тень снимается при обрыве (ревью 09a)", () => {
    const { relay, toPage } = setupRelay();
    const first = new FakeBackgroundPort();
    relay.acceptBackgroundPort(first);
    beginStarted(relay, first, 1);
    const second = new FakeBackgroundPort();
    relay.acceptBackgroundPort(second); // старой тени ушёл end_handoff
    second.command({ kind: "begin_handoff", request_id: 2, document: "doc-a", element: 3 });
    // MAIN-мир по порядку: хвост старой передачи, её handoff_ended, потом ответ новой.
    relay.handlePageMessage({ kind: "page_seek", target_us: 1 });
    relay.handlePageMessage({ kind: "handoff_ended", cause: "returned" });
    relay.handlePageMessage({ kind: "handoff_began", request_id: 2, outcome: "started" });
    relay.handlePageMessage({ kind: "page_seek", target_us: 2 });
    assert.deepEqual(second.received, [
      { kind: "handoff_began", request_id: 2, outcome: "started" },
      { kind: "page_seek", target_us: 2 },
    ]);
    const endsBefore = toPage.filter((message) => (message as PageCommand).kind === "end_handoff").length;
    second.drop();
    const endsAfter = toPage.filter((message) => (message as PageCommand).kind === "end_handoff").length;
    assert.equal(endsAfter, endsBefore + 1, "новая тень снята при обрыве её фона");
  });
});

describe("кандидаты видео", () => {
  it("запрос уходит MAIN-миру, ответ с тем же номером разрешает промис", async () => {
    const { relay, toPage } = setupRelay();
    const answer = relay.describeVideoCandidates();
    assert.deepEqual(toPage, [{ kind: "describe_candidates", request_id: 1 }]);
    const candidates = [{ ...DEFAULT_CANDIDATE_FACTS, document: "d0c", element: 1, media: { kind: "media_source", in_worker: false }, playing: true, visible_area_px: 100 }];
    relay.handlePageMessage({ kind: "candidates", request_id: 99, candidates: [] }); // чужой номер — мимо
    relay.handlePageMessage({ kind: "candidates", request_id: 1, candidates });
    assert.deepEqual(await answer, candidates);
  });

  it("MAIN-мир молчит — пустой список по таймауту; поздний ответ ничего не ломает", async () => {
    const { relay, clock } = setupRelay();
    const answer = relay.describeVideoCandidates();
    clock.advance(DESCRIBE_CANDIDATES_TIMEOUT_MS);
    assert.deepEqual(await answer, []);
    relay.handlePageMessage({ kind: "candidates", request_id: 1, candidates: [] });
    assert.equal(relay.counters().rejectedPageMessages, 0);
    assert.equal(clock.liveTimers(), 0);
  });

  it("кривой ответ не разрешает запрос и считается мусором", async () => {
    const { relay, clock } = setupRelay();
    const answer = relay.describeVideoCandidates();
    relay.handlePageMessage({ kind: "candidates", request_id: 1, candidates: [{ element: "x" }] });
    assert.equal(relay.counters().rejectedPageMessages, 1);
    clock.advance(DESCRIBE_CANDIDATES_TIMEOUT_MS);
    assert.deepEqual(await answer, []);
  });
});

const MSE_EVENT = { kind: "mse", event: { kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: 0 } };
const TINY_LIMITS = {
  limitsPerSecond: { mse: 3, page_seek: 2, page_playback: 2, player_seek_applied: 2, file_source: 1, hls_source: 1, shadow_alive: 2, next_episode_wait: 1, subtitle: 2, malformed: 2 },
  replayEventCredit: 0,
};
const FLOOD_NOTICE = (overBudget: string) => ({ kind: "page_flood", over_budget: overBudget });

describe("бюджет передачи на границе", () => {
  it("флуд: пересылка стоит сразу, фон получает одно донесение, байты и пульс больше не идут", () => {
    const { relay } = setupRelay(TINY_LIMITS);
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    for (let index = 0; index < 50; index += 1) relay.handlePageMessage(MSE_EVENT);
    relay.handlePageMessage({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(4) });
    relay.handlePageMessage({ kind: "shadow_alive" });
    const crossed = port.received.filter((message) => (message as { kind: string }).kind === "mse");
    assert.equal(crossed.length, 3, "до фона дошёл только лимит");
    assert.deepEqual(port.received.at(-1), FLOOD_NOTICE("mse"), "последним ушло донесение");
    assert.equal(port.received.filter((message) => (message as { kind: string }).kind === "page_flood").length, 1);
    assert.equal(relay.counters().droppedFloodEvents, 49, "47 событий mse, байты и пульс");
  });

  it("handoff_ended проходит и после флуда: иначе фон ждал бы подтверждения снятия тени", () => {
    const { relay } = setupRelay(TINY_LIMITS);
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    for (let index = 0; index < 10; index += 1) relay.handlePageMessage(MSE_EVENT);
    relay.handlePageMessage({ kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(port.received.at(-1), { kind: "handoff_ended", cause: "returned" });
  });

  it("байты бюджет не считает: потолок у них в очереди сеанса", () => {
    const { relay } = setupRelay(TINY_LIMITS);
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    for (let index = 0; index < 100; index += 1) relay.handlePageMessage({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(1) });
    assert.equal(port.received.filter((message) => (message as { kind: string }).kind === "bytes").length, 100);
    assert.equal(relay.counters().droppedFloodEvents, 0);
  });

  it("страница не может подделать донесение: из MAIN-мира оно мусор и до фона не доходит", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    const before = port.received.length;
    relay.handlePageMessage(FLOOD_NOTICE("mse"));
    assert.equal(parsePageEvent(FLOOD_NOTICE("mse")), undefined);
    assert.equal(port.received.length, before);
    assert.equal(relay.counters().rejectedPageMessages, 1);
  });

  it("мусор во время передачи тратит бюджет, а вне передачи ничего никуда не шлёт", () => {
    const { relay } = setupRelay(TINY_LIMITS);
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    for (let index = 0; index < 50; index += 1) relay.handlePageMessage({ kind: "garbage" });
    assert.deepEqual(port.received, [], "передачи нет: некому и незачем");
    beginStarted(relay, port, 1);
    for (let index = 0; index < 50; index += 1) relay.handlePageMessage({ kind: "garbage" });
    assert.deepEqual(port.received.at(-1), FLOOD_NOTICE("malformed"));
    assert.equal(port.received.filter((message) => (message as { kind: string }).kind === "page_flood").length, 1);
    assert.equal(relay.counters().rejectedPageMessages, 100, "счётчик мусора идёт по-прежнему");
  });

  it("ответы на незаданный вопрос во время передачи считаются как мусор", () => {
    const { relay } = setupRelay(TINY_LIMITS);
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    for (let index = 0; index < 10; index += 1) relay.handlePageMessage({ kind: "candidates", request_id: 999, candidates: [] });
    assert.deepEqual(port.received.at(-1), FLOOD_NOTICE("malformed"));
    assert.equal(relay.counters().rejectedPageMessages, 0, "форма верная: это не отказ формы");
  });

  it("новая передача — полный бюджет: следующий сеанс работает после флуда", () => {
    const { relay } = setupRelay(TINY_LIMITS);
    const first = new FakeBackgroundPort();
    relay.acceptBackgroundPort(first);
    beginStarted(relay, first, 1);
    for (let index = 0; index < 10; index += 1) relay.handlePageMessage(MSE_EVENT);
    relay.handlePageMessage({ kind: "handoff_ended", cause: "returned" });
    const second = new FakeBackgroundPort();
    relay.acceptBackgroundPort(second);
    beginStarted(relay, second, 2);
    relay.handlePageMessage(MSE_EVENT);
    assert.deepEqual(second.received.at(-1), MSE_EVENT);
  });

  it("живой поток: боевые лимиты не срабатывают на пике записей (246 кусков и 6 remove в секунду, 10 секунд)", () => {
    const { relay, clock } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    for (let tick = 0; tick < 2_460; tick += 1) {
      clock.advance(4);
      relay.handlePageMessage({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(1) });
      relay.handlePageMessage(MSE_EVENT);
      if (tick % 41 === 0) relay.handlePageMessage({ kind: "page_seek", target_us: 0 });
    }
    assert.equal(port.received.some((message) => (message as { kind: string }).kind === "page_flood"), false);
    assert.equal(relay.counters().droppedFloodEvents, 0);
  });

  it("повтор журнала у потолка записей проходит целиком, а флуд сверх него — нет", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    for (let index = 0; index < JOURNAL_REPLAY_EVENT_CREDIT; index += 1) relay.handlePageMessage(MSE_EVENT);
    assert.equal(port.received.some((message) => (message as { kind: string }).kind === "page_flood"), false, "настоящий повтор лимит не задевает");
    for (let index = 0; index < PAGE_EVENT_LIMITS_PER_SECOND.mse + 1; index += 1) relay.handlePageMessage(MSE_EVENT);
    assert.deepEqual(port.received.at(-1), FLOOD_NOTICE("mse"));
  });
});

describe("документ говорит о себе сам", () => {
  it("frame_host кандидата — из изолированного мира, что бы ни сказала страница", async () => {
    const { relay, toPage } = setupRelay();
    const pending = relay.describeVideoCandidates();
    const query = toPage.at(-1) as { request_id: number };
    relay.handlePageMessage({
      kind: "candidates",
      request_id: query.request_id,
      candidates: [{ document: "doc-a", element: 1, media: { kind: "none" }, playing: false, visible_area_px: 0, ...DEFAULT_CANDIDATE_FACTS, frame_host: "192.168.1.1" }],
    });
    const [candidate] = await pending;
    assert.equal(candidate?.frame_host, ownDocument.hostname);
  });

  it("referrer_origin — из изолированного мира; null страницы остаётся null (no-referrer)", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    const url = "https://cdn.example/a.mp4";
    relay.handlePageMessage({ kind: "file_source", url, referrer_origin: "https://bank.example", referrer_policy: "strict-origin-when-cross-origin" });
    assert.deepEqual(port.received.at(-1), { kind: "file_source", url, referrer_origin: ownDocument.origin, referrer_policy: "strict-origin-when-cross-origin" });
    relay.handlePageMessage({ kind: "file_source", url, referrer_origin: null, referrer_policy: "no-referrer" });
    assert.deepEqual(port.received.at(-1), { kind: "file_source", url, referrer_origin: null, referrer_policy: "no-referrer" });
  });

  it("hls_source (10a2): происхождение тоже подменяется своим, null остаётся null, событие доходит до фона", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    const url = "https://cdn.example/vod/master.m3u8";
    relay.handlePageMessage({ kind: "hls_source", url, referrer_origin: "https://bank.example", referrer_policy: "origin", stream: "live" });
    assert.deepEqual(port.received.at(-1), { kind: "hls_source", url, referrer_origin: ownDocument.origin, referrer_policy: "origin", stream: "live" });
    relay.handlePageMessage({ kind: "hls_source", url, referrer_origin: null, referrer_policy: "no-referrer", stream: "vod" });
    assert.deepEqual(port.received.at(-1), { kind: "hls_source", url, referrer_origin: null, referrer_policy: "no-referrer", stream: "vod" });
  });
});

describe("сторож следующего видео: граница с фоном (сессия 09c2)", () => {
  const heldEvent = { kind: "next_video_held", element: 4, candidates: [{ ...DEFAULT_CANDIDATE_FACTS, document: "doc-a", element: 4, media: { kind: "none" }, playing: true, visible_area_px: 1, frame_host: "forged.example" }], trigger: "next_video" };
  const release = { kind: "release_held_video", element: 4 };

  it("взвод доходит до MAIN-мира; донесение идёт к фону с хостом, прочитанным здесь, а не страницей", async () => {
    const { relay, toPage, heldReports } = setupRelay();
    relay.armNextVideo(3_000);
    assert.deepEqual(toPage, [{ kind: "arm_next_video", lease_ms: 3_000 }]);
    relay.handlePageMessage(heldEvent);
    await Promise.resolve();
    assert.equal(heldReports.length, 1);
    assert.equal(heldReports[0]?.candidates[0]?.frame_host, ownDocument.hostname);
  });

  it("без взвода страница не может разбудить фон: донесения нет, видео отпускается", () => {
    const { relay, toPage, heldReports } = setupRelay();
    relay.handlePageMessage(heldEvent);
    assert.deepEqual(heldReports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("срок взвода вышел: то же самое", () => {
    const { relay, clock, toPage, heldReports } = setupRelay();
    relay.armNextVideo(3_000);
    clock.advance(3_000);
    toPage.length = 0;
    relay.handlePageMessage(heldEvent);
    assert.deepEqual(heldReports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("страница молотит `play()`: сверх лимита фон не тревожится", () => {
    const { relay, heldReports } = setupRelay();
    relay.armNextVideo(3_000);
    for (let attempt = 0; attempt < HELD_VIDEO_REPORTS_PER_SECOND * 3; attempt += 1) relay.handlePageMessage(heldEvent);
    assert.equal(heldReports.length, HELD_VIDEO_REPORTS_PER_SECOND);
  });

  it("фон сказал «берём» — видео остаётся под тенью; «нет» — отпускается", async () => {
    const adopted = setupRelay();
    adopted.relay.armNextVideo(3_000);
    adopted.toPage.length = 0;
    adopted.relay.handlePageMessage(heldEvent);
    await Promise.resolve();
    await Promise.resolve();
    assert.deepEqual(adopted.toPage, []);
    const refused = setupRelay();
    refused.answerHeldWith(() => Promise.resolve("release"));
    refused.relay.armNextVideo(3_000);
    refused.toPage.length = 0;
    refused.relay.handlePageMessage(heldEvent);
    await Promise.resolve();
    await Promise.resolve();
    assert.deepEqual(refused.toPage, [release]);
  });

  it("ответа нет совсем (`undefined`): видео отпускается, а «берём» без `begin_handoff` страхуется сроком", async () => {
    const noReply = setupRelay();
    noReply.answerHeldWith(() => Promise.resolve(undefined as unknown as HeldVideoReply));
    noReply.relay.armNextVideo(3_000);
    noReply.toPage.length = 0;
    noReply.relay.handlePageMessage(heldEvent);
    await Promise.resolve();
    await Promise.resolve();
    assert.deepEqual(noReply.toPage, [release]);
    const adopted = setupRelay();
    adopted.relay.armNextVideo(3_000);
    adopted.toPage.length = 0;
    adopted.relay.handlePageMessage(heldEvent);
    await Promise.resolve();
    await Promise.resolve();
    adopted.clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS);
    assert.deepEqual(adopted.toPage, [release], "если фон всё же начал передачу, команда для забранного видео ничего не значит");
  });

  it("фон молчит или недоступен: страница не остаётся с молчащим видео", async () => {
    const silent = setupRelay();
    silent.answerHeldWith(() => new Promise(() => {}));
    silent.relay.armNextVideo(3_000);
    silent.toPage.length = 0;
    silent.relay.handlePageMessage(heldEvent);
    assert.deepEqual(silent.toPage, []);
    silent.clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS);
    assert.deepEqual(silent.toPage, [release]);
    const failed = setupRelay();
    failed.answerHeldWith(() => Promise.reject(new Error("no receiver")));
    failed.relay.armNextVideo(3_000);
    failed.toPage.length = 0;
    failed.relay.handlePageMessage(heldEvent);
    await Promise.resolve();
    await Promise.resolve();
    assert.deepEqual(failed.toPage, [release]);
  });
});
