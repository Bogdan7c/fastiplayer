// Фон: регистрация курьера под политику (фейковый scripting), сервис политики, метка плеера из hello.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { syncCarrierRegistration, type CarrierScriptRegistry, type RegisteredCarrier } from "../../src/background/codec-policy/carrier-registration.ts";
import { createCodecPolicyService } from "../../src/background/codec-policy/codec-policy-service.ts";
import { CARRIER_SCRIPT_ID, refuse } from "../../src/shared/codec-policy.ts";
import { CHEAP_CODECS_KEY, CODEC_SETTING_KEY, readCachedCheapCodecs, readCodecSetting, type KeyValueStore } from "../../src/shared/codec-storage.ts";
import { createWorld, helloAccepted, lastPort } from "../host-link/support.ts";

/** Фейковый `chrome.scripting`: помнит регистрации и считает вызовы. Память «живёт» между «перезапусками» фона. */
function createFakeRegistry(initial: RegisteredCarrier[] = []) {
  const registered = new Map(initial.map((script) => [script.id, script]));
  const log: string[] = [];
  const registry: CarrierScriptRegistry = {
    list: async () => [...registered.values()],
    register: async ({ id, file }) => {
      if (registered.has(id)) throw new Error("duplicate script id");
      registered.set(id, { id, files: [file] });
      log.push(`register:${file}`);
    },
    unregister: async (id) => {
      if (!registered.delete(id)) throw new Error("nonexistent script id");
      log.push(`unregister:${id}`);
    },
  };
  return { registry, registered, log };
}

function createFakeStore(initial: Record<string, unknown> = {}) {
  const values = new Map(Object.entries(initial));
  const store: KeyValueStore = {
    get: async (keys) => Object.fromEntries(keys.filter((key) => values.has(key)).map((key) => [key, values.get(key)])),
    set: async (items) => {
      for (const [key, value] of Object.entries(items)) values.set(key, value);
    },
  };
  return { store, values };
}

describe("syncCarrierRegistration", () => {
  it("политика H.264: курьер регистрируется, повтор — без изменений", async () => {
    const { registry, registered, log } = createFakeRegistry();
    assert.equal(await syncCarrierRegistration(registry, refuse(["av1"])), "registered");
    assert.deepEqual(registered.get(CARRIER_SCRIPT_ID), { id: CARRIER_SCRIPT_ID, files: ["codec-policy-av1.js"] });
    assert.equal(await syncCarrierRegistration(registry, refuse(["av1"])), "unchanged");
    assert.deepEqual(log, ["register:codec-policy-av1.js"]);
  });

  it("смена набора: старый курьер снят, новый зарегистрирован (регистрация одна)", async () => {
    const { registry, registered } = createFakeRegistry();
    await syncCarrierRegistration(registry, refuse(["av1"]));
    assert.equal(await syncCarrierRegistration(registry, refuse(["av1", "vp9"])), "replaced");
    assert.equal(registered.size, 1);
    assert.deepEqual(registered.get(CARRIER_SCRIPT_ID)?.files, ["codec-policy-av1-vp9.js"]);
  });

  it("прозрачная политика: курьер снят; если регистрации нет — ничего не делаем и не падаем", async () => {
    const { registry, registered, log } = createFakeRegistry();
    assert.equal(await syncCarrierRegistration(registry, refuse([])), "unchanged");
    await syncCarrierRegistration(registry, refuse(["av1"]));
    assert.equal(await syncCarrierRegistration(registry, refuse([])), "unregistered");
    assert.equal(registered.size, 0);
    assert.deepEqual(log, ["register:codec-policy-av1.js", `unregister:${CARRIER_SCRIPT_ID}`]);
  });

  it("не трогает чужие регистрации с другим id", async () => {
    const { registry, registered } = createFakeRegistry([{ id: "someone-else", files: ["x.js"] }]);
    await syncCarrierRegistration(registry, refuse(["av1"]));
    await syncCarrierRegistration(registry, refuse([]));
    assert.deepEqual([...registered.keys()], ["someone-else"]);
  });
});

describe("сервис политики кодеков", () => {
  function setup(initialStore: Record<string, unknown> = {}, initialRegistry: RegisteredCarrier[] = []) {
    const fakeStore = createFakeStore(initialStore);
    const fakeRegistry = createFakeRegistry(initialRegistry);
    const warnings: string[] = [];
    const service = createCodecPolicyService({ store: fakeStore.store, registry: fakeRegistry.registry, warn: (message) => warnings.push(message) });
    return { ...fakeStore, ...fakeRegistry, service, warnings };
  }

  it("чистая установка: auto без метки — прозрачно, курьера нет", async () => {
    const { service, registered } = setup();
    await service.resync();
    assert.equal(registered.size, 0);
    assert.deepEqual(service.appliedPolicy(), { kind: "transparent" });
  });

  it("плеер сообщил {h264} при настройке auto -> курьер H.264; метка сохранена в кэше", async () => {
    const { service, registered, store } = setup();
    await service.reportPlayerCheapCodecs(new Set(["h264"] as const));
    assert.deepEqual(registered.get(CARRIER_SCRIPT_ID)?.files, ["codec-policy-av1-vp9-hevc.js"]);
    assert.deepEqual(await readCachedCheapCodecs(store), ["h264"]);
  });

  it("плеер позже сообщил пустую метку -> курьер снят", async () => {
    const { service, registered } = setup();
    await service.reportPlayerCheapCodecs(new Set(["h264"] as const));
    await service.reportPlayerCheapCodecs(new Set());
    assert.equal(registered.size, 0);
  });

  it("настройка off перекрывает метку; always_h264 работает без метки", async () => {
    const off = setup({ [CODEC_SETTING_KEY]: "off", [CHEAP_CODECS_KEY]: ["h264"] });
    await off.service.resync();
    assert.equal(off.registered.size, 0);

    const always = setup({ [CODEC_SETTING_KEY]: "always_h264" });
    await always.service.resync();
    assert.deepEqual(always.registered.get(CARRIER_SCRIPT_ID)?.files, ["codec-policy-av1-vp9-hevc-vp8.js"]);
  });

  it("prefer_vp9 при любой метке отказывает только AV1; прежнее сохранённое значение читается как раньше", async () => {
    const { service, registered, store } = setup({ [CODEC_SETTING_KEY]: "prefer_vp9" });
    await service.resync();
    assert.deepEqual(registered.get(CARRIER_SCRIPT_ID)?.files, ["codec-policy-av1.js"]);
    // Значение из первой версии (until prefer_vp9) остаётся действующим.
    await store.set({ [CODEC_SETTING_KEY]: "always_h264" });
    assert.equal(await readCodecSetting(store), "always_h264");
  });

  it("перезапуск фона: регистрация уже есть и верна — ничего не пересоздаём (идемпотентно)", async () => {
    const { service, log } = setup({ [CHEAP_CODECS_KEY]: ["h264"] }, [{ id: CARRIER_SCRIPT_ID, files: ["codec-policy-av1-vp9-hevc.js"] }]);
    await service.resync();
    await service.resync();
    assert.deepEqual(log, []);
  });

  it("перезапуск фона после смены настройки в простое: устаревшая регистрация заменяется", async () => {
    const { service, registered } = setup({ [CODEC_SETTING_KEY]: "off" }, [{ id: CARRIER_SCRIPT_ID, files: ["codec-policy-av1.js"] }]);
    await service.resync();
    assert.equal(registered.size, 0);
  });

  it("испорченное хранилище читается как значения по умолчанию", async () => {
    const { store } = setup({ [CODEC_SETTING_KEY]: "bogus", [CHEAP_CODECS_KEY]: "h264,av1" });
    assert.equal(await readCodecSetting(store), "auto");
    assert.deepEqual(await readCachedCheapCodecs(store), []);
    const partlyBad = setup({ [CHEAP_CODECS_KEY]: ["av1", "nonsense", 7, "h264"] });
    assert.deepEqual(await readCachedCheapCodecs(partlyBad.store), ["h264", "av1"]);
  });

  it("одинаковая метка повторно не пишется в хранилище", async () => {
    const { service, store } = setup();
    await service.reportPlayerCheapCodecs(new Set(["h264"] as const));
    let writes = 0;
    const original = store.set;
    store.set = async (items) => {
      writes += 1;
      await original(items);
    };
    await service.reportPlayerCheapCodecs(new Set(["h264"] as const));
    assert.equal(writes, 0);
  });

  it("сбой scripting не бросается наружу, а попадает в warn; следующая синхронизация справляется", async () => {
    const { service, registry, registered, warnings } = setup({ [CODEC_SETTING_KEY]: "always_h264" });
    const realRegister = registry.register;
    registry.register = async () => {
      throw new Error("scripting down");
    };
    await service.resync();
    assert.equal(warnings.length, 1);
    assert.match(warnings[0] ?? "", /scripting down/);
    assert.equal(service.appliedPolicy(), undefined, "не применённая политика не объявляется применённой");
    registry.register = realRegister;
    await service.resync();
    assert.equal(registered.size, 1);
  });

  it("параллельные пересчёты идут по очереди и не дублируют регистрацию", async () => {
    const { service, registered, log } = setup({ [CODEC_SETTING_KEY]: "always_h264" });
    await Promise.all([service.resync(), service.resync(), service.resync()]);
    assert.equal(registered.size, 1);
    assert.deepEqual(log, ["register:codec-policy-av1-vp9-hevc-vp8.js"]);
  });

  it("распознаёт ключи, от которых зависит политика, и только их", () => {
    const { service } = setup();
    assert.equal(service.isPolicyInputKey(CODEC_SETTING_KEY), true);
    assert.equal(service.isPolicyInputKey(CHEAP_CODECS_KEY), true);
    assert.equal(service.isPolicyInputKey("somethingElse"), false);
  });
});

describe("метка плеера из hello_accepted (host-link)", () => {
  function helloWithTokens(capabilities: string[]): unknown {
    const accepted = helloAccepted() as { player: { capabilities: string[] } };
    accepted.player.capabilities = capabilities;
    return accepted;
  }

  function worldReportingCodecs() {
    const reported: string[][] = [];
    const world = createWorld({ onPlayerCheapVideoCodecs: (families) => reported.push([...families].sort()) });
    return { world, reported };
  }

  it("cheap_codec.* разбираются, чужие метки пропускаются; зовётся на каждый принятый hello", () => {
    const { world, reported } = worldReportingCodecs();
    world.link.acquireDemand("test");
    lastPort(world).reply(helloWithTokens(["cheap_codec.h264", "feature.page_geometry", "future.thing"]));
    assert.deepEqual(reported, [["h264"]]);
  });

  it("hello без меток кодеков сообщает пустой набор («метка снята»)", () => {
    const { world, reported } = worldReportingCodecs();
    world.link.acquireDemand("test");
    lastPort(world).reply(helloWithTokens([]));
    assert.deepEqual(reported, [[]]);
  });

  it("получатель бросил — hello всё равно принят, статус connected", () => {
    const world = createWorld({
      onPlayerCheapVideoCodecs: () => {
        throw new Error("receiver broke");
      },
    });
    world.link.acquireDemand("test");
    lastPort(world).reply(helloWithTokens(["cheap_codec.h264"]));
    assert.equal(world.link.current().state, "connected");
  });

  it("отказанный hello метку не сообщает", () => {
    const { world, reported } = worldReportingCodecs();
    world.link.acquireDemand("test");
    lastPort(world).reply({ type: "refused", in_reply_to: { kind: "hello" }, reason: { kind: "player_unavailable" } });
    assert.deepEqual(reported, []);
  });
});
