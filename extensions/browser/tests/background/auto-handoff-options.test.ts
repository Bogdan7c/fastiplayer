// Страница настроек: раздел «Auto play» (12a) читает и пишет только свои поля общей настройки.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { mountAutoHandoffSettingControl } from "../../src/options/auto-handoff-setting-control.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import type { KeyValueStore } from "../../src/shared/codec-storage.ts";
import { flushPromises } from "./support.ts";

/** Минимальный элемент: ровно то, что трогает контрол. */
class FakeElement {
  id = "";
  value = "";
  checked = false;
  type = "";
  min = "";
  max = "";
  className = "";
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
  find(id: string): FakeElement {
    const found = this.id === id ? this : this.children.map((child) => child.findOrNull(id)).find((element) => element !== null);
    assert.ok(found, id);
    return found;
  }
  findOrNull(id: string): FakeElement | null {
    return this.id === id ? this : (this.children.map((child) => child.findOrNull(id)).find((element) => element !== null) ?? null);
  }
  fire(type: string): void {
    this.listeners.get(type)?.();
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

function memoryStore(initial: Record<string, unknown> = {}): { store: KeyValueStore; values: Map<string, unknown> } {
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

const RICH: AutoHandoffSettings = {
  enabled: false,
  min_duration_s: 90,
  video_filters: { ...DEFAULT_AUTO_HANDOFF_SETTINGS.video_filters, sound: { enabled: true, sites: ["c.example"] } },
  unknown_site_rule: "ask",
  site_rules: { "a.example": "never" },
  own_stop_list: ["b.example/shorts"],
};

async function mount(initial: Record<string, unknown>) {
  const container = new FakeElement("div");
  const memory = memoryStore(initial);
  await mountAutoHandoffSettingControl(container as never, memory.store);
  return { container, ...memory };
}

describe("раздел «Auto play»", () => {
  it("без настройки показывает значения по умолчанию: выключено, Play in Fastiplayer", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({});
      assert.equal(container.find("auto-handoff-enabled").checked, false);
      assert.equal(container.find("auto-handoff-unknown-site-rule").value, "always");
      assert.deepEqual(container.find("auto-handoff-unknown-site-rule").children.map((option) => option.textContent), ["Play in Fastiplayer", "Ask first"]);
    });
  });

  it("показывает сохранённое", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: { ...RICH, enabled: true } });
      assert.equal(container.find("auto-handoff-enabled").checked, true);
      assert.equal(container.find("auto-handoff-unknown-site-rule").value, "ask");
    });
  });

  it("каждый переключатель пишет только своё поле; правила сайтов, стоп-лист и фильтры видео не теряются", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      container.find("auto-handoff-enabled").checked = true;
      container.find("auto-handoff-enabled").fire("change");
      await flushPromises();
      assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), { ...RICH, enabled: true });

      container.find("auto-handoff-unknown-site-rule").value = "always";
      container.find("auto-handoff-unknown-site-rule").fire("change");
      await flushPromises();
      assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), { ...RICH, enabled: true, unknown_site_rule: "always" });
      assert.equal(container.find("auto-handoff-status").textContent, "Saved.");
    });
  });

  it("перед записью настройка перечитывается: чужое изменение соседнего поля не затирается", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      // Другая вкладка настроек поменяла правила сайтов, пока эта страница открыта.
      values.set(AUTO_HANDOFF_SETTINGS_KEY, { ...RICH, site_rules: { "z.example": "always" } });
      container.find("auto-handoff-enabled").checked = true;
      container.find("auto-handoff-enabled").fire("change");
      await flushPromises();
      assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), { ...RICH, enabled: true, site_rules: { "z.example": "always" } });
    });
  });

  it("сбой записи показывается текстом, а не глотается", async () => {
    await withFakeDocument(async () => {
      const container = new FakeElement("div");
      const failing: KeyValueStore = { get: async () => ({}), set: () => Promise.reject(new Error("quota")) };
      await mountAutoHandoffSettingControl(container as never, failing);
      container.find("auto-handoff-enabled").checked = true;
      container.find("auto-handoff-enabled").fire("change");
      await flushPromises();
      assert.match(container.find("auto-handoff-status").textContent, /Could not save: .*quota/);
    });
  });
});
