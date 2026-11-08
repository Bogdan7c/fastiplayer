// Просьба «Return» с заглушки (11a, решение 236) у пограничника: уходит фону только по порту ТЕКУЩЕЙ
// передачи, рождается в изолированном мире (MAIN-мир такую подделать не может).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createContentRelay, type BackgroundPort } from "../../src/content/relay.ts";
import { VIEWER_RETURN_REQUEST_KIND } from "../../src/shared/handoff-contract.ts";
import { parsePageEvent, parseViewerReturnRequest } from "../../src/shared/page-message-validation.ts";
import { createFakeClock } from "../background/support.ts";

class FakeBackgroundPort implements BackgroundPort {
  readonly received: unknown[] = [];
  throwOnPost = false;
  private readonly messageListeners: ((message: unknown) => void)[] = [];
  private readonly disconnectListeners: (() => void)[] = [];
  readonly onMessage = { addListener: (listener: (message: unknown) => void) => void this.messageListeners.push(listener) };
  readonly onDisconnect = { addListener: (listener: () => void) => void this.disconnectListeners.push(listener) };
  postMessage(message: unknown): void {
    if (this.throwOnPost) throw new Error("disconnected port");
    this.received.push(message);
  }
  disconnect(): void {}
  command(message: unknown): void {
    for (const listener of this.messageListeners) listener(message);
  }
  drop(): void {
    for (const listener of this.disconnectListeners) listener();
  }
}

function setupRelay() {
  const clock = createFakeClock();
  const toPage: unknown[] = [];
  const relay = createContentRelay({
    sendToPage: (message) => void toPage.push(message),
    reportHeldVideo: () => Promise.resolve("release"),
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: () => {},
    now: () => clock.scheduler.now(),
    ownDocument: () => ({ hostname: "site.example", origin: "https://site.example" }),
  });
  return { relay, toPage };
}

function beginStarted(relay: ReturnType<typeof setupRelay>["relay"], port: FakeBackgroundPort, requestId: number): void {
  port.command({ kind: "begin_handoff", request_id: requestId, document: "doc-a", element: 3 });
  relay.handlePageMessage({ kind: "handoff_began", request_id: requestId, outcome: "started" });
}

const VIEWER_RETURN = { kind: VIEWER_RETURN_REQUEST_KIND };

describe("пограничник: просьба «Return»", () => {
  it("идёт порту текущей передачи ровно в договорной форме, которую понимает фон", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    port.received.length = 0;
    assert.equal(relay.requestViewerReturn(), "sent");
    assert.deepEqual(port.received, [VIEWER_RETURN]);
    assert.deepEqual(parseViewerReturnRequest(port.received[0]), VIEWER_RETURN);
  });

  it("передачи нет (не началась или уже кончилась) — некому, фону ничего", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    assert.equal(relay.requestViewerReturn(), "no_handoff", "до handoff_began");
    beginStarted(relay, port, 1);
    relay.handlePageMessage({ kind: "handoff_ended", cause: "returned" });
    port.received.length = 0;
    assert.equal(relay.requestViewerReturn(), "no_handoff", "после handoff_ended");
    assert.deepEqual(port.received, []);
  });

  it("порт фона пропал — некому; новый порт без своей передачи просьбу тоже не получает", () => {
    const { relay, toPage } = setupRelay();
    const first = new FakeBackgroundPort();
    relay.acceptBackgroundPort(first);
    beginStarted(relay, first, 1);
    first.drop();
    const second = new FakeBackgroundPort();
    relay.acceptBackgroundPort(second);
    assert.equal(relay.requestViewerReturn(), "no_handoff");
    assert.deepEqual(second.received, []);
    // Страховка тени при пропаже фона осталась прежней: просьба её не подменяет.
    assert.equal(toPage.filter((message) => (message as { kind: string }).kind === "end_handoff").length, 1);
  });

  it("порт бросил при отправке — `port_failed`, а не молчаливый успех", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    port.throwOnPost = true;
    assert.equal(relay.requestViewerReturn(), "port_failed");
  });

  it("MAIN-мир не может прислать такую просьбу: изолированный мир отвергает её как событие страницы", () => {
    const { relay } = setupRelay();
    const port = new FakeBackgroundPort();
    relay.acceptBackgroundPort(port);
    beginStarted(relay, port, 1);
    port.received.length = 0;
    relay.handlePageMessage(VIEWER_RETURN);
    assert.equal(parsePageEvent(VIEWER_RETURN), undefined);
    assert.deepEqual(port.received, [], "до фона подделка не дошла");
    assert.equal(relay.counters().rejectedPageMessages, 1);
  });
});

describe("форма новых сообщений 11a", () => {
  it("handoff_ended{page_player_failed} — договорное событие страницы", () => {
    assert.deepEqual(parsePageEvent({ kind: "handoff_ended", cause: "page_player_failed" }), { kind: "handoff_ended", cause: "page_player_failed" });
    assert.equal(parsePageEvent({ kind: "handoff_ended", cause: "page_player_broke" }), undefined);
  });

  it("просьба «Return» — ровно одно поле `kind`", () => {
    assert.deepEqual(parseViewerReturnRequest(VIEWER_RETURN), VIEWER_RETURN);
    for (const candidate of [{ kind: VIEWER_RETURN_REQUEST_KIND, element: 3 }, { kind: "viewer_return" }, [VIEWER_RETURN_REQUEST_KIND], null, "viewer_return_requested"]) {
      assert.equal(parseViewerReturnRequest(candidate), undefined, JSON.stringify(candidate));
    }
  });
});
