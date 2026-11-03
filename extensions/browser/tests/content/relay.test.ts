// Изолированный мир: рукопожатие с MAIN-миром, пограничник (отказ мусору), пересылка в обе стороны,
// выбор кандидатов с таймаутом, страховка тени при пропаже фона.
import assert from "node:assert/strict";
import { runInNewContext } from "node:vm";
import { describe, it } from "node:test";
import { adoptPageValue, openPageChannel, type PageChannelPort } from "../../src/content/page-channel.ts";
import { createContentRelay, DESCRIBE_CANDIDATES_TIMEOUT_MS, exactBytes, type BackgroundPort } from "../../src/content/relay.ts";
import type { PageCommand } from "../../src/shared/handoff-contract.ts";
import { HANDSHAKE_MARKER_FIELD, HANDSHAKE_VERSION, PAGE_CHANNEL_READY_EVENT } from "../../src/shared/page-channel-handshake.ts";
import { parsePageEvent } from "../../src/shared/page-message-validation.ts";
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

function setupRelay() {
  const clock = createFakeClock();
  const toPage: unknown[] = [];
  const debugLines: string[] = [];
  const relay = createContentRelay({
    sendToPage: (message) => void toPage.push(message),
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: (line) => void debugLines.push(line),
  });
  return { clock, relay, toPage, debugLines };
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
    assert.deepEqual(relay.counters(), { rejectedPageMessages: 2, undeliverablePageEvents: 1, rejectedBackgroundMessages: 0 });
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
    const candidates = [{ document: "d0c", element: 1, media: { kind: "media_source" }, playing: true, visible_area_px: 100 }];
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
