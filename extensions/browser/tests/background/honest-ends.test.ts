// Честные концы передачи (сессия 09a2): что видит плеер на проводе, когда страница забрала видео
// (Д-3, 4009), когда страница сломалась сама (Д-6, 4003), когда команда странице потерялась
// (Д-7, порт мёртв) и когда тень замолчала (Д-2, пульс).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { SOCKET_HIGH_WATER_BYTES } from "../../src/background/handoff/constants.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import { SHADOW_PULSE_INTERVAL_MS, SHADOW_SILENCE_LIMIT_MS, type PageCommand } from "../../src/shared/handoff-contract.ts";
import { deliverStream, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const SNAP_BACK_AS_SHADOW: PageCommand = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } };

/** Сокет плеера придерживает очередь (буфер сокета выше порога) — проверяем, что конец её дописывает. */
function holdSocketQueue(world: World): void {
  world.sockets.last().bufferedAmount = SOCKET_HIGH_WATER_BYTES + 1;
}

function wireHasTabGone(world: World): boolean {
  return world.sockets.last().wireSummary().includes("tab_gone");
}

function pulse(world: World): void {
  world.framePorts.last().deliver({ kind: "shadow_alive" });
}

describe("Д-3: страница забрала видео (решение 179)", () => {
  it("handoff_ended{element_released} — очередь дописана, без tab_gone, закрытие 4009", async () => {
    const world = setup();
    await startHandoff(world);
    holdSocketQueue(world);
    deliverStream(world);
    assert.deepEqual(world.sockets.last().wireSummary(), ["attach"], "очередь придержана");
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    const socket = world.sockets.last();
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "source_buffer_added", "frame 1/0#0"]);
    assert.equal(wireHasTabGone(world), false, "вкладка на месте — tab_gone был бы ложью");
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
    assert.equal(world.door.demandHeld(), 0);
  });
});

describe("Д-6: сбой внутри страницы", () => {
  it("handoff_ended{returned} без нашего end_handoff — очередь дописана, закрытие 4003, не молчаливое 1000", async () => {
    const world = setup();
    await startHandoff(world);
    holdSocketQueue(world);
    deliverStream(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    const socket = world.sockets.last();
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "source_buffer_added", "frame 1/0#0"]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(world.ended, [{ kind: "page_side_failure" }]);
    assert.equal(world.door.demandHeld(), 0);
  });
});

describe("Д-7: команда странице потерялась (порт мёртв, onDisconnect не пришёл)", () => {
  for (const lost of ["seek", "clock"] as const) {
    it(`postMessage бросает на ${lost} — сеанс кончается сразу, плеер получает честный конец`, async () => {
      const world = setup();
      await startHandoff(world);
      // Вкладка ушла на другой сайт: ждать видео нового документа нечего (16c), конец — прежний и сразу.
      world.navigateTab("https://other-site.test/");
      const port = world.framePorts.last();
      port.failPostsWhere = (command) => command.kind === "shadow" && command.command.kind === lost;
      const socket = world.sockets.last();
      if (lost === "seek") socket.receive({ type: "seek", seek_id: 7, target_us: 60_000_000 });
      else socket.receive({ type: "clock", seq: 1, position_us: 1_000_000, state: "playing", rate: 1000 });
      // Только микрозадачи (вопрос «жива ли вкладка»), часы не двигаем: ни 60 с подтверждения
      // перемотки, ни 5 с ожидания снятия тени сеанс не ждёт.
      await flushPromises();
      assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
      assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
      assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
      assert.equal(world.door.demandHeld(), 0);
      assert.equal(world.clock.liveTimers(), 0, "после конца таймеров сеанса не осталось");
    });
  }

  it("потери команд подряд, пока ждём ответа «жива ли вкладка», — один вопрос и один конец", async () => {
    // Настройка 0 (16c): ожидания после перезагрузки нет, путь прежний — вопрос «жива ли вкладка».
    const world = setup(undefined, undefined, undefined, { pageReloadWait: { seconds: 0 } });
    await startHandoff(world);
    world.framePorts.last().failPostsWhere = () => true;
    const socket = world.sockets.last();
    for (let seq = 1; seq <= 4; seq += 1) socket.receive({ type: "clock", seq, position_us: seq * 250_000, state: "playing", rate: 1000 });
    await flushPromises();
    assert.equal(world.tabExistsQuestions(), 1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.equal(socket.wireSummary().filter((kind) => kind === "tab_gone").length, 1);
  });

  it("begin_handoff не ушёл — выданный сеанс бросается сразу, а не через 5 с ожидания ответа", async () => {
    const world = setup();
    world.framePorts.failPostsOfNewPortsWhere = (command) => command.kind === "begin_handoff";
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    const socket = world.sockets.last();
    socket.open();
    socket.receive({ type: "attached" });
    assert.deepEqual(await Promise.race([outcome, flushPromises().then(() => "pending")]), { kind: "failed", reason: "page_navigated_away" });
    assert.deepEqual(socket.sentMessages().at(-1), { type: "tab_gone", reason: "navigated_away" });
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.tab_gone);
    assert.equal(world.door.demandHeld(), 0);
  });

  it("порт мёртв, а вкладку закрыли — tab_gone{closed}", async () => {
    const world = setup();
    await startHandoff(world);
    world.closeTab();
    world.framePorts.last().failPostsWhere = () => true;
    world.sockets.last().receive({ type: "pause" });
    await flushPromises();
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "tab_gone", reason: "closed" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "closed" }]);
  });
});

describe("Д-2: пульс тени (решение 180)", () => {
  it("пульсы были, потом тишина (потерянный handoff_ended) — за предел тишины снятие «как тень» и 4003", async () => {
    const world = setup();
    await startHandoff(world);
    holdSocketQueue(world);
    deliverStream(world);
    for (let second = 0; second < 5; second += 1) {
      world.clock.advance(SHADOW_PULSE_INTERVAL_MS);
      pulse(world);
    }
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS - 1);
    assert.deepEqual(world.ended, [], "до предела тишины сеанс жив");
    world.clock.advance(1);
    const socket = world.sockets.last();
    assert.deepEqual(world.framePorts.last().posted.at(-1), SNAP_BACK_AS_SHADOW);
    assert.deepEqual(socket.wireSummary(), ["attach", "generation_started", "source_buffer_added", "frame 1/0#0"]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(world.ended, [{ kind: "shadow_silent" }]);
    assert.equal(world.door.demandHeld(), 0);
    assert.equal(world.framePorts.last().disconnectedByBackground, true, "порт закрыт — изолированный мир снимет тень сам");
  });

  it("тень не прислала ни одного пульса с начала передачи — тоже конец за предел тишины", async () => {
    const world = setup();
    await startHandoff(world);
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS);
    assert.deepEqual(world.ended, [{ kind: "shadow_silent" }]);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
  });

  it("регулярный пульс держит сеанс сколь угодно долго", async () => {
    const world = setup();
    await startHandoff(world);
    // Десять минут пульса раз в секунду.
    for (let second = 0; second < 600; second += 1) {
      world.clock.advance(SHADOW_PULSE_INTERVAL_MS);
      pulse(world);
    }
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.equal(world.framePorts.last().posted.some((command) => command.kind === "end_handoff"), false);
  });

  it("байты страницы — не признак жизни тени: без пульса сеанс кончается и при живом потоке", async () => {
    const world = setup();
    await startHandoff(world);
    deliverStream(world);
    const port = world.framePorts.last();
    for (let second = 0; second < SHADOW_SILENCE_LIMIT_MS / SHADOW_PULSE_INTERVAL_MS; second += 1) {
      world.clock.advance(SHADOW_PULSE_INTERVAL_MS);
      port.deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([4, 5, 6]) });
    }
    assert.deepEqual(world.ended, [{ kind: "shadow_silent" }]);
  });

  it("пульс плееру не пересылается", async () => {
    const world = setup();
    await startHandoff(world);
    pulse(world);
    assert.deepEqual(world.sockets.last().wireSummary(), ["attach"]);
  });

  it("пульс после конца игнорируется, таймеров не остаётся", async () => {
    const world = setup();
    await startHandoff(world);
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS);
    pulse(world);
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS * 3);
    assert.deepEqual(world.ended, [{ kind: "shadow_silent" }]);
    assert.equal(world.clock.liveTimers(), 0);
  });

  it("обычный конец (return_to_page) снимает сторожа: тишина после него ничего не ломает", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "return_to_page", position_us: 1, resume: "paused" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS * 2);
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
    assert.equal(world.clock.liveTimers(), 0);
  });
});

describe("сон машины посреди просмотра (16d2, решение 330): тишина — прожитые секунды", () => {
  /** Столько T480s просыпается с идущими часами, пока браузер ещё заморожен (замер 16d2). */
  const RESUME_WITH_RUNNING_CLOCKS_MS = 48_000;

  it("часы ушли на 48 с, пока фон был заморожен — сеанс жив, пульс после пробуждения держит его", async () => {
    const world = setup();
    await startHandoff(world);
    deliverStream(world);
    for (let second = 0; second < 3; second += 1) {
      world.clock.advance(SHADOW_PULSE_INTERVAL_MS);
      pulse(world);
    }
    world.clock.thawAfterFreeze(RESUME_WITH_RUNNING_CLOCKS_MS);
    assert.deepEqual(world.ended, [], "проспанное время — не тишина страницы");
    assert.equal(world.sockets.last().closedWithCode, undefined);
    // Первый тик часов плеера после оттаивания вернул пульс — дальше сеанс живёт как обычно.
    world.clock.advance(250);
    pulse(world);
    for (let second = 0; second < 30; second += 1) {
      world.clock.advance(SHADOW_PULSE_INTERVAL_MS);
      pulse(world);
    }
    assert.deepEqual(world.ended, []);
    assert.equal(world.framePorts.last().posted.some((command) => command.kind === "end_handoff"), false);
  });

  it("после сна страница правда молчит — конец за предел прожитых секунд, а не сразу", async () => {
    const world = setup();
    await startHandoff(world);
    pulse(world);
    world.clock.thawAfterFreeze(RESUME_WITH_RUNNING_CLOCKS_MS);
    // Тик, просроченный во сне, засчитан одной секундой; ещё восемь — всё ещё в пределе.
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS - 2 * SHADOW_PULSE_INTERVAL_MS);
    assert.deepEqual(world.ended, [], "девять прожитых тихих секунд — ещё не конец");
    world.clock.advance(SHADOW_PULSE_INTERVAL_MS);
    assert.deepEqual(world.ended, [{ kind: "shadow_silent" }]);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.equal(world.clock.liveTimers(), 0, "после конца сторож не оставил таймеров");
  });
});
