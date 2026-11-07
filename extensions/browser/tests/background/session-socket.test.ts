// Клиент WebSocket сеанса на фейковом сокете: attach первым, порядок, «полон»/«принимаю»,
// обратное давление, потолок очереди, tab_gone, закрытие с кодом, мусор от плеера.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MAX_SESSION_QUEUE_BYTES, SOCKET_DRAIN_POLL_MS, SOCKET_HIGH_WATER_BYTES } from "../../src/background/handoff/constants.ts";
import type { OutgoingItem } from "../../src/background/handoff/page-event-translator.ts";
import { NORMAL_CLOSE_CODE, openSessionSocket, type SocketEndCause } from "../../src/background/handoff/session-socket.ts";
import { ATTACH_REPLY_TIMEOUT_MS } from "../../src/background/handoff/constants.ts";
import { SESSION_CLOSE_CODES, type PlayerSessionMessage } from "../../src/protocol/index.ts";
import { createFakeClock, recordSockets, TEST_PORT, TEST_TOKEN, type FakeWebSocket } from "./support.ts";

const SESSION_KEY = "t7-1-abc";

function frameItem(sequence: bigint, payload: Uint8Array = new Uint8Array([1, 2, 3])): OutgoingItem {
  return { kind: "frame", header: { kind: "source_buffer_bytes", generation: 1, source_buffer: 0, sequence }, payload };
}

const generationStarted: OutgoingItem = { kind: "message", message: { type: "generation_started", generation: 1 } };
const sourceBufferAdded: OutgoingItem = {
  kind: "message",
  message: { type: "source_buffer_added", generation: 1, source_buffer: 0, mime_type: 'video/mp4; codecs="avc1"' },
};
const pageSeek: OutgoingItem = { kind: "message", message: { type: "seek_started", target_us: 5_000_000, cause: { kind: "page_seek" } } };

function setup() {
  const clock = createFakeClock();
  const sockets = recordSockets();
  const ended: SocketEndCause[] = [];
  const playerMessages: PlayerSessionMessage[] = [];
  let attachedCalls = 0;
  const session = openSessionSocket({
    port: TEST_PORT,
    sessionKey: SESSION_KEY,
    token: TEST_TOKEN,
    createWebSocket: sockets.create,
    scheduler: clock.scheduler,
    callbacks: {
      onAttached: () => void (attachedCalls += 1),
      onPlayerMessage: (message) => void playerMessages.push(message),
      onEnded: (cause) => void ended.push(cause),
    },
  });
  const socket: FakeWebSocket = sockets.last();
  const attach = (): void => {
    socket.open();
    socket.receive({ type: "attached" });
  };
  return { clock, socket, session, ended, playerMessages, attachedCalls: () => attachedCalls, attach };
}

describe("сокет сеанса: attach и порядок", () => {
  it("идёт на 127.0.0.1:<порт>, первым шлёт attach, до attached не шлёт больше ничего", () => {
    const { socket, session, attachedCalls } = setup();
    assert.equal(socket.url, `ws://127.0.0.1:${TEST_PORT}/`);
    assert.equal(socket.binaryType, "arraybuffer");
    session.enqueue(generationStarted);
    session.enqueue(sourceBufferAdded);
    session.enqueue(frameItem(0n));
    assert.deepEqual(socket.sent, [], "до открытия сокета ничего не уходит");
    socket.open();
    assert.deepEqual(socket.sentMessages(), [{ type: "attach", session_key: SESSION_KEY, token: TEST_TOKEN }]);
    assert.equal(attachedCalls(), 0);
    socket.receive({ type: "attached" });
    assert.equal(attachedCalls(), 1);
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "source_buffer_added", "frame 1/0#0"]);
  });

  it("сокет открыт, но attached ещё нет: поставленное ждёт и уходит только после attached", () => {
    const { socket, session } = setup();
    socket.open();
    session.enqueue(generationStarted);
    session.enqueue(frameItem(0n));
    assert.deepEqual(socket.wireSummary(), ["attach"]);
    socket.receive({ type: "attached" });
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "frame 1/0#0"]);
  });

  it("после attached сообщения и рамки уходят ровно в порядке постановки", () => {
    const { socket, session, attach } = setup();
    attach();
    session.enqueue(generationStarted);
    session.enqueue(sourceBufferAdded);
    session.enqueue(frameItem(0n));
    session.enqueue(pageSeek);
    session.enqueue(frameItem(1n));
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "source_buffer_added", "frame 1/0#0", "seek_started", "frame 1/0#1"]);
  });

  it("сообщение плеера до attached — нарушение: закрытие 4003", () => {
    const { socket, ended } = setup();
    socket.open();
    socket.receive({ type: "play" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.equal(ended.length, 1);
    assert.equal(ended[0]?.kind, "player_protocol_violation");
  });

  it("нет attached за таймаут — сеанс кончается attach_timeout", () => {
    const { socket, clock, ended } = setup();
    socket.open();
    clock.advance(ATTACH_REPLY_TIMEOUT_MS - 1);
    assert.deepEqual(ended, []);
    clock.advance(1);
    assert.deepEqual(ended, [{ kind: "attach_timeout" }]);
    assert.equal(socket.closedWithCode, NORMAL_CLOSE_CODE);
  });
});

describe("сокет сеанса: «полон» и обратное давление", () => {
  it("buffer_state{full} придерживает рамки и всё за ними, accepting отпускает в том же порядке", () => {
    const { socket, session, attach } = setup();
    attach();
    session.enqueue(sourceBufferAdded);
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    session.enqueue(pageSeek); // текст до первой рамки уходит
    session.enqueue(frameItem(0n));
    session.enqueue(generationStarted); // стоит за рамкой — ждёт вместе с ней
    assert.deepEqual(socket.wireSummary(), ["attach", "source_buffer_added", "seek_started"]);
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 10_000_000, intake: "accepting" });
    assert.deepEqual(socket.wireSummary(), ["attach", "source_buffer_added", "seek_started", "frame 1/0#0", "generation_started"]);
  });

  it("«полон», оставшийся от MSE-поколения, не держит байты файла и HLS (рамки вида 2) и их итоги", () => {
    const { socket, session, attach } = setup();
    attach();
    // Реклама на MSE оставила плеер «полным», потом страница перешла на файл или HLS. Для таких
    // поколений плеер `buffer_state` не шлёт: держи мы рамки вида 2, байты не ушли бы никогда.
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    session.enqueue({ kind: "message", message: { type: "generation_started", generation: 2 } });
    session.enqueue({ kind: "frame", header: { kind: "range_bytes", request_id: 7, offset: 0 }, payload: new Uint8Array([9, 9]) });
    session.enqueue({
      kind: "message",
      message: { type: "resource_fetch_finished", request_id: 7, outcome: { kind: "completed", size_bytes: 2 } },
    });
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "range 7@0+2", "resource_fetch_finished"]);
  });

  it("за придержанной рамкой MSE ждут и рамки вида 2: порядок страницы сохраняется", () => {
    const { socket, session, attach } = setup();
    attach();
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    session.enqueue(frameItem(0n));
    session.enqueue({ kind: "frame", header: { kind: "range_bytes", request_id: 3, offset: 0 }, payload: new Uint8Array([1]) });
    assert.deepEqual(socket.wireSummary(), ["attach"]);
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 10_000_000, intake: "accepting" });
    assert.deepEqual(socket.wireSummary(), ["attach", "frame 1/0#0", "range 3@0+1"]);
  });

  it("выше порога bufferedAmount не шлёт, а ждёт, пока буфер сокета не уйдёт", () => {
    const { socket, session, attach, clock } = setup();
    attach();
    socket.bufferedAmount = SOCKET_HIGH_WATER_BYTES + 1;
    session.enqueue(frameItem(0n));
    assert.deepEqual(socket.wireSummary(), ["attach"]);
    clock.advance(SOCKET_DRAIN_POLL_MS);
    assert.deepEqual(socket.wireSummary(), ["attach"], "буфер всё ещё полон");
    socket.bufferedAmount = 0;
    clock.advance(SOCKET_DRAIN_POLL_MS);
    assert.deepEqual(socket.wireSummary(), ["attach", "frame 1/0#0"]);
    assert.equal(session.queuedBytes(), 0);
  });

  it("переполнение очереди (страница не слушает «полон») — честное закрытие 4003, не молчаливый выброс", () => {
    const { socket, session, attach, ended } = setup();
    attach();
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    const chunk = new Uint8Array(1024 * 1024);
    const fitting = Math.floor(MAX_SESSION_QUEUE_BYTES / (chunk.length + 24));
    for (let index = 0; index < fitting; index += 1) session.enqueue(frameItem(BigInt(index), chunk));
    assert.deepEqual(ended, [], "до потолка сеанс жив");
    session.enqueue(frameItem(BigInt(fitting), chunk));
    assert.deepEqual(ended, [{ kind: "queue_overflow" }]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.equal(session.queuedBytes(), 0, "очередь освобождена");
    session.enqueue(frameItem(0n));
    assert.equal(ended.length, 1, "после конца сеанс больше ничего не принимает и не сообщает");
  });
});

describe("сокет сеанса: конец", () => {
  it("tab_gone: сначала очередь, потом tab_gone, закрытие кодом 4002", () => {
    const { socket, session, attach, ended } = setup();
    attach();
    socket.bufferedAmount = SOCKET_HIGH_WATER_BYTES + 1; // очередь стоит из-за буфера сокета
    session.enqueue(frameItem(0n));
    session.sendTabGoneAndClose("navigated_away");
    assert.deepEqual(socket.wireSummary(), ["attach", "frame 1/0#0", "tab_gone"]);
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.deepEqual(ended, [{ kind: "closed_by_extension" }]);
  });

  it("tab_gone при «полон»: придержанные рамки не шлются (дыры номеров нет — новых рамок не будет)", () => {
    const { socket, session, attach } = setup();
    attach();
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    session.enqueue(frameItem(0n));
    session.sendTabGoneAndClose("closed");
    assert.deepEqual(socket.wireSummary(), ["attach", "tab_gone"]);
  });

  it("закрытие с причиной после очереди (страница забрала видео): очередь, потом закрытие 4009, без tab_gone", () => {
    const { socket, session, attach, ended } = setup();
    attach();
    socket.bufferedAmount = SOCKET_HIGH_WATER_BYTES + 1; // очередь стоит из-за буфера сокета
    session.enqueue(frameItem(0n));
    session.enqueue(pageSeek);
    session.closeAfterDeliveringQueue("page_took_video_back");
    assert.deepEqual(socket.wireSummary(), ["attach", "frame 1/0#0", "seek_started"]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(ended, [{ kind: "closed_by_extension" }]);
    session.enqueue(frameItem(1n));
    assert.deepEqual(socket.wireSummary(), ["attach", "frame 1/0#0", "seek_started"], "после закрытия ничего не уходит");
  });

  it("закрытие с причиной при «полон»: придержанные рамки не шлются, код — причина (4003)", () => {
    const { socket, session, attach } = setup();
    attach();
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 90_000_000, intake: "full" });
    session.enqueue(frameItem(0n));
    session.closeAfterDeliveringQueue("protocol_violation");
    assert.deepEqual(socket.wireSummary(), ["attach"]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
  });

  it("закрытие с причиной до attached — просто закрытие кодом причины", () => {
    const { socket, session, ended } = setup();
    socket.open();
    session.enqueue(generationStarted);
    session.closeAfterDeliveringQueue("page_took_video_back");
    assert.deepEqual(socket.wireSummary(), ["attach"], "до attached очередь не уходит и при закрытии");
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(ended, [{ kind: "closed_by_extension" }]);
  });

  it("закрытие плеером сообщает причину по коду ровно один раз", () => {
    const { socket, attach, ended } = setup();
    attach();
    socket.remoteClose(SESSION_CLOSE_CODES.replaced_by_another_tab);
    socket.remoteClose(SESSION_CLOSE_CODES.replaced_by_another_tab);
    assert.deepEqual(ended, [{ kind: "player_closed", code: 4001, reason: "replaced_by_another_tab" }]);
  });

  it("обрыв без кода протокола (1006) — player_closed без причины", () => {
    const { socket, attach, ended } = setup();
    attach();
    socket.remoteClose(1006);
    assert.deepEqual(ended, [{ kind: "player_closed", code: 1006, reason: undefined }]);
  });

  it("мусор и двоичное от плеера — закрытие 4003 с причиной", () => {
    for (const garbage of ['{"type":"clock"}', "not json", '{"type":"play","extra":1}']) {
      const { socket, attach, ended } = setup();
      attach();
      socket.receive(garbage);
      assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation, garbage);
      assert.equal(ended[0]?.kind, "player_protocol_violation", garbage);
    }
    const { socket, attach, ended } = setup();
    attach();
    socket.receiveBinary(new ArrayBuffer(4));
    assert.equal(ended[0]?.kind, "player_protocol_violation");
  });

  it("сообщения плеера после attached доходят владельцу, buffer_state — нет", () => {
    const { socket, attach, playerMessages } = setup();
    attach();
    socket.receive({ type: "buffer_state", generation: 1, ahead_us: 1, intake: "accepting" });
    socket.receive({ type: "clock", seq: 1, position_us: 2_000_000, state: "playing", rate: 1000 });
    assert.deepEqual(playerMessages, [{ type: "clock", seq: 1, position_us: 2_000_000, state: "playing", rate: 1000 }]);
  });

  it("сообщение, не проходящее строгую запись протокола, закрывает сеанс, а не уходит битым", () => {
    const { socket, session, attach, ended } = setup();
    attach();
    session.enqueue({ kind: "message", message: { type: "source_buffer_added", generation: 1, source_buffer: 0, mime_type: "" } });
    assert.equal(ended[0]?.kind, "page_message_rejected");
    assert.deepEqual(socket.wireSummary(), ["attach"]);
  });
});
