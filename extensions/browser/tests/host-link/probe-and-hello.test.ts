// Проба наличия посредника, hello и повторы: проверяем, что ушло в порт, какие статусы получили подписчики
// и в какие моменты фейкового времени были connect.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  TEST_EXTENSION_VERSION,
  createWorld,
  helloAccepted,
  lastPort,
  refusedHello,
} from "./support.ts";

describe("проба наличия посредника", () => {
  it("посредника нет: порт закрылся с ошибкой в окне пробы -> host_missing, без повторов", () => {
    const world = createWorld();
    world.link.probeHostPresence();
    lastPort(world).drop("Specified native messaging host not found.");
    assert.deepEqual(world.statuses, [{ state: "host_missing" }]);
    world.advance(120_000);
    assert.equal(world.ports.length, 1);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("connectNative бросил исключение -> host_missing", () => {
    const world = createWorld();
    world.failConnectWith = "permission denied";
    world.link.probeHostPresence();
    assert.deepEqual(world.statuses, [{ state: "host_missing" }]);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("посредник есть: порт жив всё окно -> закрыт нами, ни одного сообщения, статус остаётся unknown", () => {
    const world = createWorld();
    world.link.probeHostPresence();
    world.advance(999);
    assert.equal(lastPort(world).closedByLink, false);
    world.advance(1);
    const port = lastPort(world);
    assert.equal(port.closedByLink, true);
    assert.deepEqual(port.sentMessages, []); // плеер не разбужен
    assert.deepEqual(world.link.current(), { state: "unknown" });
    assert.deepEqual(world.statuses, []);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("попап открылся посреди пробы: тот же порт используется под hello, второго connect нет", () => {
    const world = createWorld();
    world.link.probeHostPresence();
    world.advance(100);
    world.link.acquireDemand("popup");
    assert.equal(world.ports.length, 1);
    assert.deepEqual(lastPort(world).sentControl().map((message) => message.type), ["hello"]);
    world.advance(5_000); // окно пробы прошло, но порт не закрыт: он уже под hello
    assert.equal(lastPort(world).closedByLink, false);
  });
});

describe("hello", () => {
  it("нужда -> hello точной формы -> connected с версией плеера", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    const port = lastPort(world);
    assert.deepEqual(port.sentControl(), [
      // Экстеншен говорит на своей версии (1.3, сессия 11a); общую версию называет плеер.
      { type: "hello", protocol: { major: 1, minor: 4 }, software_version: TEST_EXTENSION_VERSION, capabilities: [] },
    ]);
    port.reply(helloAccepted("0.9.0"));
    assert.deepEqual(world.statuses, [
      { state: "checking" },
      { state: "connected", playerVersion: "0.9.0", agreedProtocol: { major: 1, minor: 0 } },
    ]);
    assert.equal(world.link.current().state, "connected");
  });

  it("incompatible_version -> incompatible без повторов", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    lastPort(world).reply(refusedHello("incompatible_version", { update: "player" }));
    assert.deepEqual(world.link.current(), { state: "incompatible", update: "player" });
    world.advance(300_000);
    assert.equal(world.ports.length, 1);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("player_not_responding -> player_unavailable и повтор через 0,5 с", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    lastPort(world).reply(refusedHello("player_not_responding"));
    assert.deepEqual(world.link.current(), { state: "player_unavailable", reason: "player_not_responding" });
    world.advance(499);
    assert.equal(world.ports.length, 1);
    world.advance(1);
    assert.equal(world.ports.length, 2);
    assert.deepEqual(lastPort(world).sentControl().map((message) => message.type), ["hello"]);
  });

  it("нет ответа 8 с -> no_reply, порт закрыт нами, повтор", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    world.advance(7_999);
    assert.equal(world.link.current().state, "checking");
    world.advance(1);
    assert.deepEqual(world.link.current(), { state: "player_unavailable", reason: "no_reply" });
    assert.equal(world.ports[0]?.closedByLink, true);
    world.advance(500);
    assert.deepEqual(world.connectTimes(), [0, 8_500]);
  });

  it("мусорный ответ -> не падение, статус no_reply", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    lastPort(world).reply({ unexpected: true });
    assert.deepEqual(world.link.current(), { state: "player_unavailable", reason: "no_reply" });
    lastPort(world).reply("не объект"); // уже закрытый порт: игнор
  });

  it("одинаковый статус подряд подписчикам не рассылается", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    lastPort(world).reply(refusedHello("player_not_responding"));
    world.advance(500);
    lastPort(world).reply(refusedHello("player_not_responding"));
    world.advance(1_000);
    lastPort(world).reply(refusedHello("player_unavailable"));
    assert.deepEqual(world.statuses, [
      { state: "checking" },
      { state: "player_unavailable", reason: "player_not_responding" },
      { state: "player_unavailable", reason: "player_unavailable" },
    ]);
  });
});

describe("host_missing против player_unavailable", () => {
  it("порт закрылся молча в первую секунду -> host_missing", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    world.advance(50);
    lastPort(world).drop("Native host has exited.");
    assert.deepEqual(world.link.current(), { state: "host_missing" });
  });

  it("порт закрылся молча позже секунды -> no_reply (посредник работал)", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    world.advance(2_000);
    lastPort(world).drop();
    assert.deepEqual(world.link.current(), { state: "player_unavailable", reason: "no_reply" });
  });

  it("connect бросает при нужде -> host_missing и повторы идут", () => {
    const world = createWorld();
    world.failConnectWith = "boom";
    world.link.acquireDemand("popup");
    assert.deepEqual(world.link.current(), { state: "host_missing" });
    world.failConnectWith = undefined;
    world.advance(500);
    lastPort(world).reply(helloAccepted());
    assert.equal(world.link.current().state, "connected");
  });
});

describe("повторы", () => {
  it("плеер пропал -> паузы 0,5/1/2/4 -> плеер вернулся -> connected -> паузы сброшены", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    lastPort(world).reply(helloAccepted());
    world.advance(100);
    lastPort(world).drop(); // плеер пропал: порт закрылся после успешного ответа
    assert.deepEqual(world.link.current(), { state: "player_unavailable", reason: "no_reply" });

    world.advance(500); // попытка 1 в 600
    lastPort(world).reply(refusedHello("player_unavailable"));
    world.advance(1_000); // попытка 2 в 1600
    lastPort(world).reply(refusedHello("player_unavailable"));
    world.advance(2_000); // попытка 3 в 3600
    lastPort(world).reply(refusedHello("player_unavailable"));
    world.advance(4_000); // попытка 4 в 7600
    lastPort(world).reply(helloAccepted("1.0.0"));
    assert.deepEqual(world.connectTimes(), [0, 600, 1_600, 3_600, 7_600]);
    assert.equal(world.link.current().state, "connected");

    world.advance(100);
    lastPort(world).drop();
    world.advance(499);
    assert.equal(world.ports.length, 5);
    world.advance(1); // после успеха пауза снова 0,5 с: 7700 + 500
    assert.deepEqual(world.connectTimes(), [0, 600, 1_600, 3_600, 7_600, 8_200]);
  });

  it("паузы растут до потолка 30 с и дальше не растут", () => {
    const world = createWorld();
    world.link.acquireDemand("popup");
    const expectedDelays = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000];
    for (const delay of expectedDelays) {
      lastPort(world).reply(refusedHello("player_unavailable"));
      world.advance(delay - 1);
      const before = world.ports.length;
      world.advance(1);
      assert.equal(world.ports.length, before + 1, `повтор через ${delay} мс`);
    }
  });

  it("нужда снята посреди повторов -> больше ни одного connect, таймеров нет", () => {
    const world = createWorld();
    const release = world.link.acquireDemand("popup");
    lastPort(world).reply(refusedHello("player_unavailable"));
    world.advance(300);
    release();
    assert.equal(world.liveTimerCount(), 0);
    world.advance(600_000);
    assert.equal(world.ports.length, 1);
  });

  it("вторая нужда держит повторы, пока не сняты обе; повторное освобождение игнорируется", () => {
    const world = createWorld();
    const first = world.link.acquireDemand("popup");
    const second = world.link.acquireDemand("video");
    lastPort(world).reply(refusedHello("player_unavailable"));
    first();
    first(); // вторично — без эффекта
    world.advance(500);
    assert.equal(world.ports.length, 2);
    second();
    lastPort(world).reply(refusedHello("player_unavailable"));
    world.advance(600_000);
    assert.equal(world.ports.length, 2);
  });
});

describe("простой", () => {
  it("без нужды порт закрывается через 30 с, статус connected сбрасывается в unknown", () => {
    const world = createWorld();
    const release = world.link.acquireDemand("popup");
    lastPort(world).reply(helloAccepted());
    release();
    world.advance(29_999);
    assert.equal(lastPort(world).closedByLink, false);
    world.advance(1);
    assert.equal(lastPort(world).closedByLink, true);
    assert.deepEqual(world.link.current(), { state: "unknown" });
    assert.equal(world.liveTimerCount(), 0);
  });

  it("нужда вернулась до закрытия: порт жив и hello уходит в него повторно (проверка по-настоящему)", () => {
    const world = createWorld();
    const release = world.link.acquireDemand("popup");
    lastPort(world).reply(helloAccepted());
    release();
    world.advance(20_000);
    world.link.acquireDemand("popup");
    world.advance(5_000); // простой (30 с) уже прошёл бы, но порт держит нужда; ответ на второй hello ждём ≤ 8 с
    lastPort(world).reply(helloAccepted());
    world.advance(60_000);
    assert.equal(world.ports.length, 1);
    assert.deepEqual(world.ports[0]?.sentControl().map((message) => message.type), ["hello", "hello"]);
  });
});
