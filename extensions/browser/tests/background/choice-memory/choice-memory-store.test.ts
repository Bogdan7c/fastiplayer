// Хранилище памяти выбора (14b2): слияние записи, касание, вытеснение по объёму, «Forget» в очереди с идущей
// записью, битое хранилище и сбой записи.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createChoiceMemoryStore, withinCapacity } from "../../../src/background/choice-memory/choice-memory-store.ts";
import { REMEMBERED_CHOICES_KEY, type RememberedChoices } from "../../../src/shared/choice-memory-contract.ts";
import { CHOICE_MEMORY_SETTINGS_KEY } from "../../../src/shared/choice-memory-settings.ts";
import type { KeyValueStore } from "../../../src/shared/codec-storage.ts";
import { memoryStore } from "../auto-handoff-lists-fake-dom.ts";

const A = "a".repeat(64);
const B = "b".repeat(64);
const C = "c".repeat(64);

function storeAt(initial: Record<string, unknown> = {}) {
  const storage = memoryStore(initial);
  let clock = 1_000;
  const memory = createChoiceMemoryStore({ store: storage.store, now: () => clock });
  return { storage, memory, setNow: (value: number) => void (clock = value) };
}

/** Записи памяти, как они лежат в хранилище. */
const seriesIn = (values: Map<string, unknown>): Record<string, RememberedChoices> =>
  (values.get(REMEMBERED_CHOICES_KEY) as { series: Record<string, RememberedChoices> } | undefined)?.series ?? {};

describe("запись и чтение", () => {
  it("remember создаёт запись, второй remember меняет только своё поле и время касания", async () => {
    const { storage, memory, setNow } = storeAt();
    await memory.remember(A, { audio: "Dub B" });
    setNow(2_000);
    await memory.remember(A, { quality: "720p" });
    assert.deepEqual(seriesIn(storage.values), { [A]: { audio: "Dub B", quality: "720p", used_at: 2_000 } });
    assert.deepEqual(await memory.recall(A), { audio: "Dub B", quality: "720p", used_at: 2_000 });
    assert.equal(await memory.recall(B), undefined);
  });

  it("recall не меняет время касания, touch меняет; touch несуществующей записи ничего не пишет", async () => {
    const { storage, memory, setNow } = storeAt();
    await memory.remember(A, { audio: "Dub B" });
    setNow(5_000);
    await memory.recall(A);
    assert.equal(seriesIn(storage.values)[A]?.used_at, 1_000);
    await memory.touch(A);
    assert.equal(seriesIn(storage.values)[A]?.used_at, 5_000);
    const writesBefore = storage.state.writes;
    await memory.touch(B);
    assert.equal(storage.state.writes, writesBefore);
  });

  it("битый документ в хранилище читается как пустая память, годные записи соседей не теряются", async () => {
    const { memory } = storeAt({
      [REMEMBERED_CHOICES_KEY]: { version: 1, series: { [A]: { audio: "Dub B", used_at: 3 }, nothex: { audio: "x", used_at: 1 }, [B]: { used_at: "late" } } },
    });
    assert.equal(await memory.count(), 1);
    const { memory: foreign } = storeAt({ [REMEMBERED_CHOICES_KEY]: { version: 99, series: { [A]: { used_at: 1 } } } });
    assert.equal(await foreign.count(), 0);
  });
});

describe("объём", () => {
  it("сверх объёма уходят те, кого дольше всего не касались; только что записанная остаётся", async () => {
    const { storage, memory, setNow } = storeAt({ [CHOICE_MEMORY_SETTINGS_KEY]: { capacity: 10 } });
    // Десять сериалов с нарастающим временем, потом одиннадцатый.
    const fingerprints = Array.from({ length: 11 }, (_, index) => index.toString(16).padStart(64, "0"));
    for (const [index, fingerprint] of fingerprints.entries()) {
      setNow(100 + index);
      await memory.remember(fingerprint, { audio: `Dub ${index}` });
    }
    const kept = Object.keys(seriesIn(storage.values));
    assert.equal(kept.length, 10);
    assert.ok(!kept.includes(fingerprints[0] as string), "самый давний вытеснен");
    assert.ok(kept.includes(fingerprints[10] as string), "только что записанный остался");
  });

  it("только что записанная остаётся, даже если её время отстало (часы фона пошли назад)", () => {
    const series = { [A]: { used_at: 900 }, [B]: { used_at: 800 }, [C]: { used_at: 10 } };
    assert.deepEqual(Object.keys(withinCapacity(series, 2, C)).sort(), [A, C].sort());
  });
});

describe("очередь и сбои", () => {
  it("«Forget», пришедший во время записи, выполняется после неё: память пуста, запись не воскресает", async () => {
    const storage = memoryStore();
    let releaseWrite: () => void = () => {};
    let writesStarted = 0;
    const slowStore: KeyValueStore = {
      get: storage.store.get,
      set: async (items) => {
        writesStarted += 1;
        if (writesStarted === 1) await new Promise<void>((resolve) => (releaseWrite = resolve));
        await storage.store.set(items);
      },
    };
    const memory = createChoiceMemoryStore({ store: slowStore, now: () => 1 });
    const writing = memory.remember(A, { audio: "Dub B" });
    const forgetting = memory.forget();
    await new Promise((resolve) => setImmediate(resolve));
    releaseWrite();
    await Promise.all([writing, forgetting]);
    assert.deepEqual(seriesIn(storage.values), {});
    assert.equal(await memory.count(), 0);
  });

  it("сбой записи достаётся вызывающему, а следующие действия очереди идут", async () => {
    const { storage, memory } = storeAt();
    storage.state.failWrites = true;
    await assert.rejects(memory.remember(A, { audio: "Dub B" }), /write broke/u);
    storage.state.failWrites = false;
    await memory.remember(B, { audio: "Dub C" });
    assert.deepEqual(Object.keys(seriesIn(storage.values)), [B]);
  });
});
