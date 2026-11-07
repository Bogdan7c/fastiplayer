// openSession: сопоставление по request_id, повтор после hello_required, явные отказы вместо зависаний.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createWorld,
  helloAccepted,
  lastPort,
  refusedHello,
  refusedOpenSession,
  sampleRequest,
  sessionGranted,
} from "./support.ts";

function typesSent(world: ReturnType<typeof createWorld>): string[] {
  return lastPort(world)
    .sentControl()
    .map((message) => message.type);
}

describe("openSession", () => {
  it("hello -> open_session -> grant; ушедший запрос принимает разбор протокола", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    assert.deepEqual(typesSent(world), ["hello"]);
    port.reply(helloAccepted());
    assert.deepEqual(port.sentControl()[1], { type: "open_session", request_id: 1, ...sampleRequest("key-a") });
    port.reply(sessionGranted(1, "key-a"));
    const result = await outcome;
    assert.equal(result.kind, "granted");
    if (result.kind === "granted") {
      assert.equal(result.grant.port, 4000);
      assert.equal(result.grant.outcome, "started");
    }
    // Запросов не осталось, нужды нет: порт закроется по простою, других таймеров нет.
    world.advance(30_000);
    assert.equal(port.closedByLink, true);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("выдача несёт версию протокола, о которой договорились в hello этого соединения", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    port.reply(helloAccepted("0.9.0", { major: 1, minor: 1 }));
    port.reply(sessionGranted(1, "key-a"));
    const result = await outcome;
    assert.ok(result.kind === "granted");
    assert.deepEqual(result.agreedProtocol, { major: 1, minor: 1 });
  });

  it("старый плеер договорился на 1.0: выдача несёт 1.0, а не версию экстеншена", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    port.reply(helloAccepted("0.9.0", { major: 1, minor: 0 }));
    port.reply(sessionGranted(1, "key-a"));
    const result = await outcome;
    assert.ok(result.kind === "granted");
    assert.deepEqual(result.agreedProtocol, { major: 1, minor: 0 });
  });

  it("hello_required: версия берётся из повторного hello, не из первого", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    port.reply(helloAccepted("0.9.0", { major: 1, minor: 0 }));
    port.reply(refusedOpenSession(1, "hello_required"));
    port.reply(helloAccepted("0.9.0", { major: 1, minor: 1 }));
    port.reply(sessionGranted(1, "key-a"));
    const result = await outcome;
    assert.ok(result.kind === "granted");
    assert.deepEqual(result.agreedProtocol, { major: 1, minor: 1 });
  });

  it("hello_required: сами повторяем hello и тот же open_session один раз -> grant", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    port.reply(helloAccepted());
    port.reply(refusedOpenSession(1, "hello_required"));
    assert.deepEqual(typesSent(world), ["hello", "open_session", "hello"]);
    port.reply(helloAccepted());
    assert.deepEqual(typesSent(world), ["hello", "open_session", "hello", "open_session"]);
    const messages = port.sentControl();
    assert.deepEqual(messages[3], messages[1]); // тот же запрос с тем же request_id
    port.reply(sessionGranted(1, "key-a"));
    assert.equal((await outcome).kind, "granted");
    assert.equal(world.ports.length, 1); // тот же порт
  });

  it("два hello_required подряд -> отказ, без петли", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest());
    const port = lastPort(world);
    port.reply(helloAccepted());
    port.reply(refusedOpenSession(1, "hello_required"));
    port.reply(helloAccepted());
    port.reply(refusedOpenSession(1, "hello_required"));
    const result = await outcome;
    assert.equal(result.kind, "refused");
    if (result.kind === "refused") assert.equal(result.refusal.reason.kind, "hello_required");
    assert.deepEqual(typesSent(world), ["hello", "open_session", "hello", "open_session"]);
  });

  it("порт упал при ожидании ответа -> явный отказ no_reply", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest());
    lastPort(world).reply(helloAccepted());
    world.advance(2_000);
    lastPort(world).drop();
    assert.deepEqual(await outcome, { kind: "no_reply" });
    // Нужды нет — повторов нет, таймеров нет.
    world.advance(600_000);
    assert.equal(world.ports.length, 1);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("ответы двух запросов не путаются (request_id)", async () => {
    const world = createWorld();
    const first = world.link.openSession(sampleRequest("key-1"));
    const second = world.link.openSession(sampleRequest("key-2"));
    const port = lastPort(world);
    port.reply(helloAccepted());
    assert.deepEqual(
      port.sentControl().map((message) => (message.type === "open_session" ? message.request_id : 0)),
      [0, 1, 2],
    );
    port.reply(sessionGranted(2, "key-2"));
    port.reply(refusedOpenSession(1, "protected_content"));
    const [firstResult, secondResult] = [await first, await second];
    assert.equal(secondResult.kind, "granted");
    assert.equal(firstResult.kind, "refused");
    if (firstResult.kind === "refused") assert.equal(firstResult.refusal.reason.kind, "protected_content");
  });

  it("связь не поднялась -> not_connected со статусом, без висящих повторов", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest());
    lastPort(world).drop("Specified native messaging host not found.");
    assert.deepEqual(await outcome, { kind: "not_connected", status: { state: "host_missing" } });
    world.advance(600_000);
    assert.equal(world.ports.length, 1);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("плеер отказал на hello -> not_connected с причиной", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest());
    lastPort(world).reply(refusedHello("player_unavailable"));
    assert.deepEqual(await outcome, {
      kind: "not_connected",
      status: { state: "player_unavailable", reason: "player_unavailable" },
    });
  });

  it("плеер молчит на open_session 12 с -> no_reply, порт остаётся живым", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest());
    lastPort(world).reply(helloAccepted());
    world.advance(11_999);
    world.advance(1);
    assert.deepEqual(await outcome, { kind: "no_reply" });
    assert.equal(lastPort(world).closedByLink, false);
  });

  it("запрос, не прошедший строгую проверку протокола -> invalid_request без обращения к посреднику", async () => {
    const world = createWorld();
    const result = await world.link.openSession(sampleRequest("ключ с пробелом"));
    assert.equal(result.kind, "invalid_request");
    assert.equal(world.ports.length, 0);
    assert.equal(world.liveTimerCount(), 0);
  });

  it("плеер перезапустился при двух запросах в полёте: оба повторяются после ОДНОГО нового hello", async () => {
    const world = createWorld();
    const first = world.link.openSession(sampleRequest("key-1"));
    const second = world.link.openSession(sampleRequest("key-2"));
    const port = lastPort(world);
    port.reply(helloAccepted());
    // Ответы идут в порядке отправки: hello_required на оба запроса раньше ответа на новый hello.
    port.reply(refusedOpenSession(1, "hello_required"));
    port.reply(refusedOpenSession(2, "hello_required"));
    assert.deepEqual(typesSent(world), ["hello", "open_session", "open_session", "hello"]);
    port.reply(helloAccepted());
    const resent = port
      .sentControl()
      .slice(4)
      .map((message) => (message.type === "open_session" ? message.request_id : 0));
    assert.deepEqual(resent, [1, 2]); // оба запроса ушли заново, каждый один раз
    port.reply(sessionGranted(1, "key-1"));
    port.reply(sessionGranted(2, "key-2"));
    assert.equal((await first).kind, "granted");
    assert.equal((await second).kind, "granted");
  });

  it("попап открыт, пока запрос ждёт ответа: лишнего hello нет, запрос не обрывается", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    port.reply(helloAccepted());
    const releasePopup = world.link.acquireDemand("popup");
    assert.deepEqual(typesSent(world), ["hello", "open_session"]);
    // Дольше таймаута hello (8 с), но меньше таймаута запроса (12 с): запрос жив.
    world.advance(9_000);
    assert.equal(port.closedByLink, false);
    port.reply(sessionGranted(1, "key-a"));
    assert.equal((await outcome).kind, "granted");
    assert.equal(world.link.current().state, "connected");
    releasePopup();
  });

  it("ответ с чужим session_key не принимается за grant", async () => {
    const world = createWorld();
    const outcome = world.link.openSession(sampleRequest("key-a"));
    const port = lastPort(world);
    port.reply(helloAccepted());
    port.reply(sessionGranted(1, "other-key"));
    port.reply(sessionGranted(1, "key-a"));
    assert.equal((await outcome).kind, "granted");
  });
});
