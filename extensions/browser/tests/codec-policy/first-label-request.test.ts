// Первый вопрос плееру о метке кодеков (13a, решение 266): связь берётся один раз, пока метка
// неизвестна, отпускается по ответу или по сроку; при известной метке плеер не будится.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { CarrierScriptRegistry, RegisteredCarrier } from "../../src/background/codec-policy/carrier-registration.ts";
import { createCodecPolicyService } from "../../src/background/codec-policy/codec-policy-service.ts";
import { createFirstLabelRequest, FIRST_LABEL_WAIT_MS } from "../../src/background/codec-policy/first-label-request.ts";
import { PORT_IDLE_CLOSE_MS } from "../../src/background/host-link/constants.ts";
import { CARRIER_SCRIPT_ID } from "../../src/shared/codec-policy.ts";
import { CHEAP_CODECS_KEY, type KeyValueStore } from "../../src/shared/codec-storage.ts";
import { createWorld, helloAccepted, lastPort } from "../host-link/support.ts";

/** Хранилище в памяти; считает записи. */
function createMemoryStore(initial: Record<string, unknown> = {}) {
  const values = new Map(Object.entries(initial));
  let writes = 0;
  const store: KeyValueStore = {
    get: async (keys) => Object.fromEntries(keys.filter((key) => values.has(key)).map((key) => [key, values.get(key)])),
    set: async (items) => {
      writes += 1;
      for (const [key, value] of Object.entries(items)) values.set(key, value);
    },
  };
  return { store, values, writes: () => writes };
}

/** `chrome.scripting` в памяти. */
function createMemoryRegistry() {
  const registered = new Map<string, RegisteredCarrier>();
  const registry: CarrierScriptRegistry = {
    list: async () => [...registered.values()],
    register: async ({ id, file }) => {
      registered.set(id, { id, files: [file] });
    },
    unregister: async (id) => {
      registered.delete(id);
    },
  };
  return { registry, registered };
}

/** Фон в сборе, как в `background/main.ts`: связь, сервис политики и первый вопрос. */
function backgroundWorld(initialStore: Record<string, unknown> = {}) {
  const memory = createMemoryStore(initialStore);
  const scripting = createMemoryRegistry();
  const policy = createCodecPolicyService({ store: memory.store, registry: scripting.registry, warn: () => {} });
  const pendingReports: Promise<void>[] = [];
  const world = createWorld({
    onPlayerCheapVideoCodecs: (families) => {
      pendingReports.push(policy.reportPlayerCheapCodecs(families));
      firstLabel.playerAnswered();
    },
  });
  const firstLabel = createFirstLabelRequest({
    knowsPlayerLabel: () => policy.knowsPlayerLabel(),
    acquireDemand: (reason) => world.link.acquireDemand(reason),
    scheduler: world.scheduler,
  });
  return { world, firstLabel, policy, memory, scripting, settled: () => Promise.all(pendingReports) };
}

function helloWithCodecs(tokens: string[]): unknown {
  const accepted = helloAccepted() as { player: { capabilities: string[] } };
  accepted.player.capabilities = tokens;
  return accepted;
}

describe("первый вопрос о метке кодеков", () => {
  it("метка неизвестна: hello уходит сразу, ответ даёт курьера H.264 до первой страницы, связь отпускается", async () => {
    const { world, firstLabel, memory, scripting, settled } = backgroundWorld();
    await firstLabel.askIfUnknown();

    assert.equal(world.ports.length, 1, "фон должен сам открыть связь, не дожидаясь передачи видео");
    assert.deepEqual(
      lastPort(world).sentControl().map((message) => message.type),
      ["hello"],
    );

    lastPort(world).reply(helloWithCodecs(["cheap_codec.h264"]));
    await settled();
    assert.deepEqual(memory.values.get(CHEAP_CODECS_KEY), ["h264"]);
    // Политика применена: первая же страница получит курьера, запрещающего AV1/VP9/HEVC.
    assert.deepEqual(scripting.registered.get(CARRIER_SCRIPT_ID)?.files, ["codec-policy-av1-vp9-hevc.js"]);

    // Нужда снята: порт закрывается простоем, повторов связи нет.
    world.advance(PORT_IDLE_CLOSE_MS);
    assert.equal(lastPort(world).closedByLink, true);
    world.advance(10 * FIRST_LABEL_WAIT_MS);
    assert.equal(world.ports.length, 1);
  });

  it("пустая метка («ограничений нет») запоминается: следующий старт браузера плеер не будит", async () => {
    const { world, firstLabel, memory, settled } = backgroundWorld();
    await firstLabel.askIfUnknown();
    lastPort(world).reply(helloWithCodecs([]));
    await settled();
    assert.deepEqual(memory.values.get(CHEAP_CODECS_KEY), [], "пустая метка обязана попасть в кэш");

    world.advance(PORT_IDLE_CLOSE_MS);
    await firstLabel.askIfUnknown();
    assert.equal(world.ports.length, 1, "известная метка — второго вопроса нет");
  });

  it("метка уже в кэше: вопрос не задаётся, порт не открывается", async () => {
    const { world, firstLabel } = backgroundWorld({ [CHEAP_CODECS_KEY]: ["h264"] });
    await firstLabel.askIfUnknown();
    assert.equal(world.ports.length, 0);
  });

  it("плеер не ответил: через срок нужда отпускается и повторы связи прекращаются", async () => {
    const { world, firstLabel } = backgroundWorld();
    await firstLabel.askIfUnknown();
    // Посредника нет: браузер закрывает порт, связь уходит в повторы, пока держится нужда.
    lastPort(world).drop("Specified native messaging host not found.");
    world.advance(FIRST_LABEL_WAIT_MS);
    const portsAtDeadline = world.ports.length;
    world.advance(10 * FIRST_LABEL_WAIT_MS);
    assert.equal(world.ports.length, portsAtDeadline, "после срока связь не должна дёргаться");
    assert.equal(world.liveTimerCount(), 0, "висящих таймеров не остаётся");
  });

  it("два события подряд (onInstalled и onStartup) задают один вопрос", async () => {
    const { world, firstLabel } = backgroundWorld();
    await Promise.all([firstLabel.askIfUnknown(), firstLabel.askIfUnknown()]);
    assert.equal(world.ports.length, 1);
    assert.equal(lastPort(world).sentControl().filter((message) => message.type === "hello").length, 1);
  });

  it("ответ на чужой hello (передача видео) не отпускает чужую нужду", async () => {
    const { world, firstLabel } = backgroundWorld({ [CHEAP_CODECS_KEY]: ["h264"] });
    const releaseHandoff = world.link.acquireDemand("handoff");
    lastPort(world).reply(helloWithCodecs(["cheap_codec.h264"]));
    // Первый вопрос не задавался — playerAnswered ничего не отпускает, связь передачи жива.
    firstLabel.playerAnswered();
    world.advance(PORT_IDLE_CLOSE_MS);
    assert.equal(lastPort(world).closedByLink, false);
    releaseHandoff();
  });
});
