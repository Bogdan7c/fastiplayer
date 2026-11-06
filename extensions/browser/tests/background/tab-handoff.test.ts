// Передача вкладки целиком на фейках: опрос фреймов → выбор → open_session → WebSocket → attach →
// begin_handoff → поток → команды плеера → конец (возврат, отказ поколения, обрыв, уход со страницы).
// Честные концы со стороны страницы (09a2: 4009, 4003, мёртвый порт, пульс) — honest-ends.test.ts.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { HANDOFF_BEGIN_REPLY_TIMEOUT_MS, HANDOFF_END_REPLY_TIMEOUT_MS } from "../../src/background/handoff/constants.ts";
import { NORMAL_CLOSE_CODE } from "../../src/background/handoff/session-socket.ts";
import type { PageCommand } from "../../src/shared/handoff-contract.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import { deliverStream, MP4, PLAYING_VIDEO, setup, startHandoff, TAB } from "./handoff-world.ts";
import { flushPromises, grantFor, TEST_TOKEN } from "./support.ts";

describe("передача вкладки: старт", () => {
  it("сначала плеер, потом страница: open_session → attach → begin_handoff → started", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    assert.deepEqual(world.door.requests, [
      {
        session_key: `t${TAB}-1-test`,
        page: { title: "Серія 3 Озвучка", domain: "www.example.com" },
        intent: "window",
        hints: { duration: { kind: "unknown" }, position: { kind: "unknown" } },
      },
    ]);
    assert.equal(world.framePorts.ports.length, 0, "до attached страницу не трогаем");
    const socket = world.sockets.last();
    socket.open();
    socket.receive({ type: "attached" });
    const port = world.framePorts.last();
    assert.equal(port.frameId, 0);
    assert.deepEqual(port.posted, [{ kind: "begin_handoff", request_id: 1, document: "doc-7f3a", element: 4 }]);
    port.deliver({ kind: "handoff_began", request_id: 1, outcome: "started" });
    assert.deepEqual(await outcome, { kind: "started", player_window: "new" });
    assert.equal(world.door.demandHeld(), 1, "нужда связи держится всё время сеанса");
  });

  it("плеер открыл новое окно -> new; плеер вернул уже живой сеанс (reused_existing) -> already_open (решение 216)", async () => {
    const reused = setup();
    reused.door.nextOutcome = (request) => grantFor(request, "reused_existing");
    assert.deepEqual(await startHandoff(reused), { kind: "started", player_window: "already_open" });
    const fresh = setup();
    assert.deepEqual(await startHandoff(fresh), { kind: "started", player_window: "new" });
  });

  it("видео другой вкладки при открытом окне плеера: плеер заменил окно (replaced_previous) -> already_open", async () => {
    const world = setup();
    world.door.nextOutcome = (request) => grantFor(request, "replaced_previous");
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "already_open" });
  });

  it("повторная команда после старта отвечает already_open: окно уже открыто", async () => {
    const world = setup();
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "started", player_window: "already_open" });
  });

  it("onSessionStarted: слушатель зовётся ровно при удаче, не при отказе; его сбой передачу не ломает", async () => {
    const started = setup();
    const heard: number[] = [];
    started.handoffs.onSessionStarted(() => {
      throw new Error("listener broke");
    });
    started.handoffs.onSessionStarted((tabId) => void heard.push(tabId));
    assert.deepEqual(await startHandoff(started), { kind: "started", player_window: "new" });
    assert.deepEqual(heard, [TAB]);

    const refused = setup();
    const refusedHeard: number[] = [];
    refused.handoffs.onSessionStarted((tabId) => void refusedHeard.push(tabId));
    refused.door.nextOutcome = () => ({
      kind: "refused",
      refusal: { type: "refused", in_reply_to: { kind: "open_session", request_id: 1 }, reason: { kind: "player_not_responding" } },
    });
    assert.equal((await refused.handoffs.handOffTab(TAB)).kind, "player_unavailable");
    assert.deepEqual(refusedHeard, []);
  });

  it("отказ плеера не трогает страницу и отдаёт причину", async () => {
    const world = setup();
    world.door.nextOutcome = () => ({
      kind: "refused",
      refusal: { type: "refused", in_reply_to: { kind: "open_session", request_id: 1 }, reason: { kind: "player_not_responding" } },
    });
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "player_unavailable", reason: "player_not_responding" });
    assert.equal(world.framePorts.ports.length, 0);
    assert.equal(world.sockets.sockets.length, 0);
    assert.equal(world.door.demandHeld(), 0, "нужда освобождена");
  });

  it("сокет закрылся до attached — player_unavailable, страница не тронута", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().remoteClose(SESSION_CLOSE_CODES.invalid_token);
    assert.deepEqual(await outcome, { kind: "player_unavailable", reason: "session_socket:invalid_token" });
    assert.equal(world.framePorts.ports.length, 0);
  });

  it("нет видео с живым MediaSource — no_video без open_session", async () => {
    const world = setup([{ frameId: 0, result: [{ ...PLAYING_VIDEO, media: { kind: "none" } }] }, { frameId: 3, result: null }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "no_video" });
    assert.equal(world.door.requests.length, 0);
    assert.equal(world.door.demandAcquired(), 0);
  });

  it("элемент исчез к старту (или фрейм уже на другом документе) — no_video, плееру tab_gone{navigated_away}", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: 1, outcome: "element_not_found" });
    assert.deepEqual(await outcome, { kind: "no_video" });
    assert.deepEqual(world.sockets.last().wireSummary(), ["attach", "tab_gone"]);
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.equal(world.framePorts.last().disconnectedByBackground, true);
    assert.equal(world.door.demandHeld(), 0);
  });

  it("вкладку закрыли, пока ждали attached — исход сразу, плеер после attach получает tab_gone{closed}", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.handoffs.tabRemoved(TAB);
    assert.deepEqual(await outcome, { kind: "failed", reason: "tab_closed" });
    assert.equal(world.sockets.last().closedWithCode, undefined, "сокет ждёт attached, а не рвётся молча");
    world.sockets.last().receive({ type: "attached" });
    assert.deepEqual(world.sockets.last().sentMessages(), [
      { type: "attach", session_key: `t${TAB}-1-test`, token: TEST_TOKEN },
      { type: "tab_gone", reason: "closed" },
    ]);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.equal(world.framePorts.ports.length, 0, "страница не тронута");
    assert.equal(world.door.demandHeld(), 0);
    assert.deepEqual(world.ended, [{ kind: "start_failed", outcome: { kind: "failed", reason: "tab_closed" } }]);
  });

  it("вкладку закрыли, пока плеер выдавал сеанс — выданный сеанс не остаётся сиротой: attach → tab_gone{closed}", async () => {
    const world = setup();
    let grant: (() => void) | undefined;
    const originalOpen = world.door.openSession;
    world.door.openSession = (request) =>
      new Promise((resolve) => {
        grant = () => resolve(originalOpen(request));
      });
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    assert.ok(grant !== undefined, "open_session отправлен");
    world.handoffs.tabRemoved(TAB);
    grant();
    await flushPromises();
    const socket = world.sockets.last();
    socket.open();
    socket.receive({ type: "attached" });
    // Исход уже известен (гонка с «ещё не решено», чтобы поломка падала сразу, а не висела).
    assert.deepEqual(await Promise.race([outcome, flushPromises().then(() => "pending")]), { kind: "failed", reason: "tab_closed" });
    assert.deepEqual(socket.wireSummary(), ["attach", "tab_gone"]);
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "closed" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.equal(world.framePorts.ports.length, 0);
    assert.equal(world.door.demandHeld(), 0);
  });

  it("сеанс брошен, а плеер так и не ответил attached — конец по закрытию сокета, нужда освобождена", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.handoffs.tabRemoved(TAB);
    await outcome;
    world.sockets.last().remoteClose(SESSION_CLOSE_CODES.attach_timeout);
    assert.equal(world.door.demandHeld(), 0);
    assert.equal(world.ended.length, 1);
  });

  it("вкладку закрыли до ответа опроса фреймов — сеанс не открывается", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    world.handoffs.tabRemoved(TAB);
    await flushPromises();
    assert.deepEqual(await outcome, { kind: "failed", reason: "tab_closed" });
    assert.equal(world.sockets.sockets.length, 0);
  });

  it("страница молчит на begin_handoff — failed по таймауту, порт закрыт, плееру tab_gone", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    world.clock.advance(HANDOFF_BEGIN_REPLY_TIMEOUT_MS);
    assert.deepEqual(await outcome, { kind: "failed", reason: "page_did_not_answer" });
    assert.equal(world.framePorts.last().disconnectedByBackground, true);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.tab_gone);
  });

  it("повторная команда во время передачи отдаёт тот же исход; после конца — новая передача с новым ключом", async () => {
    const world = setup();
    const first = startHandoff(world);
    const again = world.handoffs.handOffTab(TAB);
    assert.deepEqual(await first, { kind: "started", player_window: "new" });
    assert.deepEqual(await again, { kind: "started", player_window: "new" });
    assert.equal(world.surveys(), 1, "второй опрос не делался");
    assert.equal(world.sockets.sockets.length, 1);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    assert.equal(world.door.requests[1]?.session_key, `t${TAB}-2-test`);
  });
});

describe("передача вкладки: поток и команды", () => {
  it("события страницы уходят в сокет поколениями и рамками в порядке страницы", async () => {
    const world = setup();
    await startHandoff(world);
    deliverStream(world);
    world.framePorts.last().deliver({ kind: "page_seek", target_us: 3_000_000 });
    assert.deepEqual(world.sockets.last().wireSummary(), ["attach", "generation_started", "source_buffer_added", "frame 1/0#0", "seek_started"]);
  });

  it("мусор от изолированного мира в сокет не попадает", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "page_seek", target_us: -1 });
    world.framePorts.last().deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: [1, 2] });
    assert.deepEqual(world.sockets.last().wireSummary(), ["attach"]);
  });

  it("команды плеера становятся командами тени", async () => {
    const world = setup();
    await startHandoff(world);
    const socket = world.sockets.last();
    socket.receive({ type: "clock", seq: 1, position_us: 1_500_000, state: "playing", rate: 1000 });
    socket.receive({ type: "pause" });
    socket.receive({ type: "seek", seek_id: 5, target_us: 60_000_000 });
    socket.receive({ type: "set_rate", rate: 2000 });
    socket.receive({ type: "play" });
    assert.deepEqual(world.framePorts.last().posted.slice(1), [
      { kind: "shadow", command: { kind: "clock", position_us: 1_500_000, state: "playing", rate: 1000 } },
      { kind: "shadow", command: { kind: "pause" } },
      { kind: "shadow", command: { kind: "seek", seek_id: 5, target_us: 60_000_000 } },
      { kind: "shadow", command: { kind: "set_rate", rate: 2000 } },
      { kind: "shadow", command: { kind: "play" } },
    ] satisfies PageCommand[]);
  });

  it("битый поток страницы: сеанс закрыт 4003, тень снимается «как тень»", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([1]) });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(world.framePorts.last().posted.at(-1), {
      kind: "end_handoff",
      position: { kind: "shadow_position" },
      resume: { kind: "as_shadow" },
    });
  });
});

describe("передача вкладки: конец", () => {
  it("return_to_page → end_handoff{at T, exactly}; подтверждение страницы закрывает сеанс и снимает нужду", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "return_to_page", position_us: 42_000_000, resume: "playing" });
    const port = world.framePorts.last();
    assert.deepEqual(port.posted.at(-1), {
      kind: "end_handoff",
      position: { kind: "at", position_us: 42_000_000 },
      resume: { kind: "exactly", resume: "playing" },
    });
    assert.equal(world.door.demandHeld(), 1, "до подтверждения страницы сеанс жив");
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    assert.equal(world.sockets.last().closedWithCode, NORMAL_CLOSE_CODE);
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.equal(world.door.demandHeld(), 0);
  });

  it("страница не подтвердила снятие — сеанс всё равно закрывается по таймауту", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "return_to_page", position_us: 1, resume: "paused" });
    world.clock.advance(HANDOFF_END_REPLY_TIMEOUT_MS);
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.equal(world.door.demandHeld(), 0);
  });

  it("снятие тени не отправилось (порт мёртв, а onDisconnect нет) — сеанс кончается сразу, не ждёт предела (Д-7)", async () => {
    const world = setup();
    await startHandoff(world);
    world.framePorts.last().disconnectedByBackground = true; // отправка в порт теперь бросает
    world.sockets.last().receive({ type: "return_to_page", position_us: 1, resume: "paused" });
    await flushPromises(); // только вопрос «жива ли вкладка», часы стоят
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.equal(world.door.demandHeld(), 0);
    assert.equal(world.clock.liveTimers(), 0, "предел ожидания снят вместе с сеансом");
  });

  it("stream_rejected текущего поколения снимает тень; старого — нет", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    deliverStream(world);
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-2", source_buffer: 0, mime_type: MP4 } });
    world.sockets.last().receive({ type: "stream_rejected", generation: 1, reason: "unsupported_codec" });
    assert.equal(port.posted.filter((command) => command.kind === "end_handoff").length, 0, "отказ старого поколения сеанс не рвёт");
    world.sockets.last().receive({ type: "stream_rejected", generation: 2, reason: "protected_content" });
    assert.deepEqual(port.posted.at(-1), { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(world.ended, [{ kind: "stream_rejected" }]);
    // Решение владельца 09a2: окно не гаснет молча — плеер покажет «Stream error — video returned
    // to the page» по коду сбоя 4003, а не по обычному 1000.
    assert.equal(world.sockets.last().closedWithCode, 4003);
  });

  it("возврат по просьбе плеера закрывает сокет обычным 1000 (конец объяснил сам плеер)", async () => {
    const world = setup();
    await startHandoff(world);
    const port = world.framePorts.last();
    world.sockets.last().receive({ type: "return_to_page", position_us: 5_000_000, resume: "paused" });
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.equal(world.sockets.last().closedWithCode, 1000);
  });

  it("обрыв WebSocket посреди передачи снимает тень на позиции тени", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().remoteClose(1006);
    assert.deepEqual(world.framePorts.last().posted.at(-1), {
      kind: "end_handoff",
      position: { kind: "shadow_position" },
      resume: { kind: "as_shadow" },
    });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(world.ended, [{ kind: "player_ended", cause: { kind: "player_closed", code: 1006, reason: undefined } }]);
  });

  it("ушли со страницы (порт закрылся, вкладка жива) — tab_gone{navigated_away}, код 4002", async () => {
    const world = setup();
    await startHandoff(world);
    deliverStream(world);
    world.framePorts.last().drop();
    await flushPromises();
    const socket = world.sockets.last();
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.equal(world.door.demandHeld(), 0);
  });

  it("вкладку закрыли (порт закрылся раньше tabs.onRemoved) — tab_gone{closed}", async () => {
    const world = setup();
    await startHandoff(world);
    world.closeTab();
    world.framePorts.last().drop();
    await flushPromises();
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "closed" });
    world.handoffs.tabRemoved(TAB);
    assert.equal(world.ended.length, 1, "конец сообщён один раз");
  });

  it("вкладку закрыли (tabs.onRemoved раньше порта) — tab_gone{closed} сразу", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.tabRemoved(TAB);
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "closed" });
    world.framePorts.last().drop();
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "closed" }]);
  });
});
