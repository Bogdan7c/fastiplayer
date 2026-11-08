// Страница настроек: раздел «Auto play» (12a) читает и пишет только свои поля общей настройки.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { mountAutoHandoffSettingControl, parseMinDurationInput } from "../../src/options/auto-handoff-setting-control.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, MAX_AUTO_HANDOFF_MIN_DURATION_S, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
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
  it("без настройки показывает значения по умолчанию: выключено, 60 секунд, Play in Fastiplayer", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({});
      assert.equal(container.find("auto-handoff-enabled").checked, false);
      assert.equal(container.find("auto-handoff-min-duration").value, "60");
      assert.equal(container.find("auto-handoff-unknown-site-rule").value, "always");
      assert.deepEqual(container.find("auto-handoff-unknown-site-rule").children.map((option) => option.textContent), ["Play in Fastiplayer", "Ask first"]);
    });
  });

  it("показывает сохранённое", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: { ...RICH, enabled: true } });
      assert.equal(container.find("auto-handoff-enabled").checked, true);
      assert.equal(container.find("auto-handoff-min-duration").value, "90");
      assert.equal(container.find("auto-handoff-unknown-site-rule").value, "ask");
    });
  });

  it("каждый переключатель пишет только своё поле; свои правила сайтов и стоп-лист не теряются", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      container.find("auto-handoff-enabled").checked = true;
      container.find("auto-handoff-enabled").fire("change");
      await flushPromises();
      assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), { ...RICH, enabled: true });

      container.find("auto-handoff-min-duration").value = "120";
      container.find("auto-handoff-min-duration").fire("change");
      await flushPromises();
      assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), { ...RICH, enabled: true, min_duration_s: 120 });

      container.find("auto-handoff-unknown-site-rule").value = "always";
      container.find("auto-handoff-unknown-site-rule").fire("change");
      await flushPromises();
      assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), { ...RICH, enabled: true, min_duration_s: 120, unknown_site_rule: "always" });
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

  it("неверный порог не пишется, поле возвращается к хранимому, человек видит подсказку", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      for (const bad of ["", "abc", "-5", "1.5", "99999", String(MAX_AUTO_HANDOFF_MIN_DURATION_S + 1)]) {
        container.find("auto-handoff-min-duration").value = bad;
        container.find("auto-handoff-min-duration").fire("change");
        await flushPromises();
        assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), RICH, bad);
        assert.equal(container.find("auto-handoff-min-duration").value, "90", bad);
        assert.match(container.find("auto-handoff-status").textContent, /whole number of seconds/);
      }
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

  it("parseMinDurationInput: границы", () => {
    assert.equal(parseMinDurationInput("0"), 0);
    assert.equal(parseMinDurationInput(" 3600 "), 3600);
    assert.equal(parseMinDurationInput("3601"), undefined);
    assert.equal(parseMinDurationInput("1e3"), undefined);
    assert.deepEqual(DEFAULT_AUTO_HANDOFF_SETTINGS.min_duration_s, 60);
  });
});
