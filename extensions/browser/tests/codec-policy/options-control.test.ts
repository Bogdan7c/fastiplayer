// Страница настроек: выбор кодека читается из хранилища и сохраняется при смене.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { mountCodecSettingControl } from "../../src/options/codec-setting-control.ts";
import { CODEC_SETTING_KEY, type KeyValueStore } from "../../src/shared/codec-storage.ts";

/** Минимальный элемент: ровно то, что трогает контрол. */
class FakeElement {
  id = "";
  value = "";
  textContent = "";
  readonly children: FakeElement[] = [];
  readonly listeners = new Map<string, () => void>();
  readonly tag: string;
  constructor(tag: string) {
    this.tag = tag;
  }
  append(...nodes: FakeElement[]): void {
    this.children.push(...nodes);
  }
  addEventListener(type: string, listener: () => void): void {
    this.listeners.set(type, listener);
  }
  find(id: string): FakeElement | undefined {
    return this.id === id ? this : this.children.map((child) => child.find(id)).find((found) => found !== undefined);
  }
}

async function withFakeDocument<T>(action: () => Promise<T>): Promise<T> {
  const globals = globalThis as unknown as { document?: unknown };
  const saved = globals.document;
  globals.document = { createElement: (tag: string) => new FakeElement(tag) };
  try {
    return await action();
  } finally {
    globals.document = saved;
  }
}

function memoryStore(initial: Record<string, unknown>): { store: KeyValueStore; values: Map<string, unknown> } {
  const values = new Map(Object.entries(initial));
  return {
    values,
    store: {
      get: async (keys) => Object.fromEntries(keys.filter((key) => values.has(key)).map((key) => [key, values.get(key)])),
      set: async (items) => {
        for (const [key, value] of Object.entries(items)) values.set(key, value);
      },
    },
  };
}

describe("контрол «Video codec for sites»", () => {
  it("показывает три варианта, выбранный берёт из хранилища (по умолчанию auto)", async () => {
    await withFakeDocument(async () => {
      for (const [stored, expected] of [[undefined, "auto"], ["always_h264", "always_h264"], ["junk", "auto"]] as const) {
        const container = new FakeElement("div");
        await mountCodecSettingControl(container as never, memoryStore(stored === undefined ? {} : { [CODEC_SETTING_KEY]: stored }).store);
        const select = container.find("codec-setting");
        assert.ok(select !== undefined);
        assert.deepEqual(select.children.map((option) => option.value), ["auto", "prefer_vp9", "always_h264", "off"]);
        assert.equal(select.value, expected);
      }
    });
  });

  it("смена выбора пишет значение в хранилище и сообщает «Saved.»", async () => {
    await withFakeDocument(async () => {
      const { store, values } = memoryStore({});
      const container = new FakeElement("div");
      await mountCodecSettingControl(container as never, store);
      const select = container.find("codec-setting")!;
      select.value = "off";
      select.listeners.get("change")?.();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(values.get(CODEC_SETTING_KEY), "off");
      assert.equal(container.find("codec-setting-status")?.textContent, "Saved.");
    });
  });

  it("сбой записи показывается текстом, а не глотается; чужое значение в select не записывается", async () => {
    await withFakeDocument(async () => {
      const failing: KeyValueStore = { get: async () => ({}), set: async () => Promise.reject(new Error("quota")) };
      const container = new FakeElement("div");
      await mountCodecSettingControl(container as never, failing);
      const select = container.find("codec-setting")!;
      select.value = "always_h264";
      select.listeners.get("change")?.();
      await new Promise((resolve) => setImmediate(resolve));
      assert.match(container.find("codec-setting-status")?.textContent ?? "", /Could not save: .*quota/);

      const { store, values } = memoryStore({});
      const second = new FakeElement("div");
      await mountCodecSettingControl(second as never, store);
      const other = second.find("codec-setting")!;
      other.value = "evil";
      other.listeners.get("change")?.();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(values.has(CODEC_SETTING_KEY), false);
    });
  });
});
