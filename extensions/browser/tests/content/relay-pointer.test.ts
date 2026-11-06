// Пограничник и вопросы про указатель (сессия 10c): `rightClickedVideo` и `videoAtPoint`.
// Итог — то, что получит вызывающий (фон или кнопка): проверенное видео или «ничего».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createContentRelay, POINTER_QUERY_TIMEOUT_MS } from "../../src/content/relay.ts";
import { createFakeClock } from "../background/support.ts";

function setupRelay() {
  const clock = createFakeClock();
  const toPage: { kind: string; request_id: number; x?: number; y?: number }[] = [];
  const relay = createContentRelay({
    sendToPage: (message) => void toPage.push(message as never),
    reportHeldVideo: () => Promise.resolve("release"),
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: () => {},
    now: () => clock.scheduler.now(),
    ownDocument: () => ({ hostname: "site.example", origin: "https://site.example" }),
  });
  return { clock, relay, toPage };
}

const AT_POINT = { document: "doc-1", element: 4, rect: { left: 1, top: 2, width: 300, height: 200 }, in_player: false };

describe("relay: видео последнего правого клика", () => {
  it("задаёт вопрос MAIN и отдаёт проверенный ответ", async () => {
    const { relay, toPage } = setupRelay();
    const answer = relay.rightClickedVideo();
    assert.deepEqual(toPage, [{ kind: "describe_right_clicked", request_id: toPage[0]?.request_id }]);
    relay.handlePageMessage({ kind: "right_clicked", request_id: toPage[0]?.request_id, video: { document: "doc-1", element: 4 } });
    assert.deepEqual(await answer, { document: "doc-1", element: 4 });
  });

  it("MAIN сказал «ничего» — null", async () => {
    const { relay, toPage } = setupRelay();
    const answer = relay.rightClickedVideo();
    relay.handlePageMessage({ kind: "right_clicked", request_id: toPage[0]?.request_id, video: null });
    assert.equal(await answer, null);
  });

  it("молчание MAIN (фрейм без нашего скрипта): по сроку — null, таймер снят", async () => {
    const { relay, clock } = setupRelay();
    const answer = relay.rightClickedVideo();
    clock.advance(POINTER_QUERY_TIMEOUT_MS);
    assert.equal(await answer, null);
    assert.equal(clock.liveTimers(), 0);
  });

  it("кривой ответ (ключ элемента 0, лишнее поле) вопрос не закрывает и считается мусором; настоящий ответ потом проходит", async () => {
    const { relay, clock, toPage } = setupRelay();
    const answer = relay.rightClickedVideo();
    const requestId = toPage[0]?.request_id;
    relay.handlePageMessage({ kind: "right_clicked", request_id: requestId, video: { document: "doc-1", element: 0 } });
    relay.handlePageMessage({ kind: "right_clicked", request_id: requestId, video: { document: "doc-1", element: 4, extra: 1 } });
    assert.equal(relay.counters().rejectedPageMessages, 2);
    relay.handlePageMessage({ kind: "right_clicked", request_id: requestId, video: { document: "doc-1", element: 4 } });
    assert.deepEqual(await answer, { document: "doc-1", element: 4 });
    assert.equal(clock.liveTimers(), 0);
  });

  it("ответ другого вида на тот же номер (подделка) вопрос не закрывает", async () => {
    const { relay, toPage, clock } = setupRelay();
    const answer = relay.rightClickedVideo();
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: toPage[0]?.request_id, video: AT_POINT });
    clock.advance(POINTER_QUERY_TIMEOUT_MS);
    assert.equal(await answer, null, "подделка не подсунула видео");
  });

  it("ответ на незаданный номер игнорируется и не ломает следующий вопрос", async () => {
    const { relay, toPage } = setupRelay();
    relay.handlePageMessage({ kind: "right_clicked", request_id: 999, video: { document: "doc-1", element: 4 } });
    const answer = relay.rightClickedVideo();
    relay.handlePageMessage({ kind: "right_clicked", request_id: toPage[0]?.request_id, video: null });
    assert.equal(await answer, null);
  });
});

describe("relay: видео под точкой (для кнопки поверх видео)", () => {
  it("шлёт координаты и отдаёт видео с рамкой и in_player", async () => {
    const { relay, toPage } = setupRelay();
    const answer = relay.videoAtPoint(120.5, 80);
    assert.deepEqual(toPage[0], { kind: "video_at_point", request_id: toPage[0]?.request_id, x: 120.5, y: 80 });
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: toPage[0]?.request_id, video: AT_POINT });
    assert.deepEqual(await answer, AT_POINT);
  });

  it("рамка с нечисловыми или отрицательными размерами отбрасывается; по сроку — null", async () => {
    const { relay, toPage, clock } = setupRelay();
    const answer = relay.videoAtPoint(1, 1);
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: toPage[0]?.request_id, video: { ...AT_POINT, rect: { left: 0, top: 0, width: -5, height: 10 } } });
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: toPage[0]?.request_id, video: { ...AT_POINT, rect: { left: 0, top: 0, width: Number.NaN, height: 10 } } });
    assert.equal(relay.counters().rejectedPageMessages, 2);
    clock.advance(POINTER_QUERY_TIMEOUT_MS);
    assert.equal(await answer, null);
  });

  it("два вопроса одновременно получают свои ответы, а не чужие", async () => {
    const { relay, toPage } = setupRelay();
    const first = relay.videoAtPoint(1, 1);
    const second = relay.videoAtPoint(2, 2);
    const [a, b] = toPage;
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: b?.request_id, video: { ...AT_POINT, element: 9 } });
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: a?.request_id, video: { ...AT_POINT, element: 8 } });
    assert.equal((await first)?.element, 8);
    assert.equal((await second)?.element, 9);
  });

  it("не трогает порт фона и состояние передачи: вопросы про указатель без порта работают", async () => {
    const { relay, toPage } = setupRelay();
    const answer = relay.videoAtPoint(1, 1);
    relay.handlePageMessage({ kind: "video_at_point_result", request_id: toPage[0]?.request_id, video: null });
    assert.equal(await answer, null);
    assert.deepEqual(relay.counters(), { rejectedPageMessages: 0, undeliverablePageEvents: 0, rejectedBackgroundMessages: 0, droppedFloodEvents: 0 });
  });
});
