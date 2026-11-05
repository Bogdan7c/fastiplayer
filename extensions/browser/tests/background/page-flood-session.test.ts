// Флуд страницы через весь фон (решение 200): донесение изолированного мира → 4003, тень снята,
// причина в модели вкладки; плеер, экстеншен и другие вкладки живы, следующий сеанс работает.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MAX_SESSION_QUEUE_BYTES, SOCKET_HIGH_WATER_BYTES } from "../../src/background/handoff/constants.ts";
import { createTabModel } from "../../src/background/tab-model/tab-model.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import { parsePageEvent, parsePageFloodNotice } from "../../src/shared/page-message-validation.ts";
import { deliverStream, MP4, setup, startHandoff, TAB } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const FLOOD_NOTICE = { kind: "page_flood", over_budget: "mse" };
const END_AS_SHADOW = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } };

describe("флуд байтов: счётчик копий в MAIN-мире", () => {
  const FLOODED = { kind: "handoff_ended", cause: "page_flood" };

  it("handoff_ended{page_flood} оставляет в модели причину page_flood, а не сбой на стороне страницы; 4003", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver(FLOODED);
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "page_flood", overBudget: "forwarded_bytes" }]);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "page_flood" });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
  });

  it("настоящий сбой на стороне страницы флудом не считается", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "page_side_failure" }]);
    assert.equal(await world.tabModel.lastRefusalOf(TAB), null);
  });
});

describe("донесение о флуде в фоне", () => {
  it("сеанс кончается 4003, тень снята «как тень», порт закрыт, отказ page_flood записан в модель", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver(FLOOD_NOTICE);
    await flushPromises();
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(port.posted.at(-1), END_AS_SHADOW);
    assert.equal(port.disconnectedByBackground, true);
    assert.deepEqual(world.ended, [{ kind: "page_flood", overBudget: "mse" }]);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "page_flood" });
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.equal(world.door.demandHeld(), 0, "нужда связи отпущена: плеер не держим");
  });

  it("причина переживает сон фона: новая модель читает её из хранилища", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver(FLOOD_NOTICE);
    await flushPromises();
    const reborn = createTabModel({ store: world.store, now: () => 0, reportStoreFailure: () => assert.fail("хранилище исправно") });
    assert.deepEqual(await reborn.lastRefusalOf(TAB), { kind: "page_flood" });
  });

  it("очередь сеанса дописана плееру до 4003: у плеера нет дыры в уже принятом", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    const socket = world.sockets.last();
    // Буфер сокета выше порога: уже принятый поток лежит в очереди, а не на проводе.
    socket.bufferedAmount = SOCKET_HIGH_WATER_BYTES + 1;
    deliverStream(world);
    assert.ok(!socket.sent.some((payload) => typeof payload !== "string"), "байты ещё в очереди");
    port.deliver(FLOOD_NOTICE);
    assert.ok(socket.sent.some((payload) => typeof payload !== "string"), "байты дописаны до закрытия");
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
  });

  it("следующий сеанс на этой вкладке открывается и играет", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver(FLOOD_NOTICE);
    await flushPromises();
    assert.deepEqual(await startHandoff(world), { kind: "started" });
    assert.equal(world.door.requests.length, 2);
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB]);
  });

  it("другая вкладка не задета: её сеанс живёт дальше", async () => {
    const world = setup();
    await startHandoff(world);
    const firstPort = world.framePorts.last();
    const firstSocket = world.sockets.last();
    const otherTab = world.handoffs.handOffTab(TAB + 1);
    await flushPromises();
    const otherSocket = world.sockets.last();
    otherSocket.open();
    otherSocket.receive({ type: "attached" });
    const otherBegin = world.framePorts.last().posted[0];
    assert.ok(otherBegin?.kind === "begin_handoff");
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: otherBegin.request_id, outcome: "started" });
    assert.deepEqual(await otherTab, { kind: "started" });
    firstPort.deliver(FLOOD_NOTICE);
    assert.equal(firstSocket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.equal(otherSocket.closedWithCode, undefined);
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB + 1]);
  });

  it("донесение после нашего end_handoff ничего не меняет: конец остаётся за плеером", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    world.sockets.last().receive({ type: "return_to_page", position_us: 0, resume: "paused" });
    await flushPromises();
    port.deliver(FLOOD_NOTICE);
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.equal(world.sockets.last().closedWithCode, 1000);
  });

  it("кривое донесение — мусор: сеанс живёт", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "page_flood" });
    port.deliver({ kind: "page_flood", over_budget: "everything" });
    port.deliver({ kind: "page_flood", over_budget: "mse", extra: 1 });
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
  });

  it("разбор: фон признаёт донесение, разбор событий страницы — нет", () => {
    assert.deepEqual(parsePageFloodNotice(FLOOD_NOTICE), FLOOD_NOTICE);
    assert.equal(parsePageEvent(FLOOD_NOTICE), undefined);
  });
});

describe("гигантские сегменты: потолок очереди", () => {
  it("страница льёт байты, плеер не принимает: 4003, тень снимается, в модели page_flood", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    const socket = world.sockets.last();
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    const chunk = new Uint8Array(32 * 1024 * 1024);
    for (let sent = 0; sent <= MAX_SESSION_QUEUE_BYTES && socket.closedWithCode === undefined; sent += chunk.length) {
      port.deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: chunk });
    }
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(port.posted.at(-1), END_AS_SHADOW);
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "player_ended", cause: { kind: "queue_overflow" } }]);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "page_flood" });
  });
});
