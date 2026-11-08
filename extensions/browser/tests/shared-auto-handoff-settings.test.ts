// Настройка режима 2 «автозапуск» (12a): разбор хранилища, границы, нормализация, пределы, чтение/запись.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { KeyValueStore } from "../src/shared/codec-storage.ts";
import {
  AUTO_HANDOFF_SETTINGS_KEY,
  DEFAULT_AUTO_HANDOFF_SETTINGS,
  MAX_AUTO_HANDOFF_MIN_DURATION_S,
  MAX_OWN_STOP_LIST_ENTRIES,
  MAX_SITE_RULES,
  autoHandoffSettingsFromChange,
  normalizeStopListEntry,
  parseAutoHandoffSettings,
  readAutoHandoffSettings,
  readAutoHandoffSettingsOrDefaults,
  siteKeyOfHost,
  wantsAutoHandoffWatch,
  writeAutoHandoffSettings,
} from "../src/shared/auto-handoff-settings.ts";

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

describe("разбор настройки режима 2", () => {
  it("не объект — значения по умолчанию: режим выключен, порог 60, новый сайт — always", () => {
    for (const garbage of [undefined, null, "x", 5, [], true]) assert.deepEqual(parseAutoHandoffSettings(garbage), DEFAULT_AUTO_HANDOFF_SETTINGS);
    assert.deepEqual(DEFAULT_AUTO_HANDOFF_SETTINGS, { enabled: false, min_duration_s: 60, unknown_site_rule: "always", site_rules: {}, own_stop_list: [] });
  });

  it("битое поле сбрасывается само по себе и не трогает соседей", () => {
    const parsed = parseAutoHandoffSettings({ enabled: true, min_duration_s: "много", unknown_site_rule: "never", site_rules: [], own_stop_list: "x" });
    assert.deepEqual(parsed, { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true });
  });

  it("порог: целое от 0 до предела; дробь, минус, NaN и сверх предела — по умолчанию", () => {
    const minOf = (value: unknown): number => parseAutoHandoffSettings({ min_duration_s: value }).min_duration_s;
    assert.equal(minOf(0), 0);
    assert.equal(minOf(MAX_AUTO_HANDOFF_MIN_DURATION_S), MAX_AUTO_HANDOFF_MIN_DURATION_S);
    for (const bad of [-1, 1.5, Number.NaN, MAX_AUTO_HANDOFF_MIN_DURATION_S + 1, "30", null]) assert.equal(minOf(bad), 60, String(bad));
  });

  it("правила сайтов: чужие значения и ключи не в нормальном виде отбрасываются, остальные живут", () => {
    const parsed = parseAutoHandoffSettings({
      site_rules: { "example.com": "always", "bad.example": "maybe", "WWW.Upper.com": "never", "www.a.com": "never", "ok.org": "ask", "": "always", "a b.com": "never" },
    });
    assert.deepEqual(parsed.site_rules, { "example.com": "always", "ok.org": "ask" });
  });

  it("свой стоп-лист: записи приводятся к виду host[/путь], повторы и мусор уходят", () => {
    const parsed = parseAutoHandoffSettings({ own_stop_list: ["https://WWW.Example.com/Shorts/", "example.com/shorts", 42, "", "a b", "news.site", "news.site"] });
    assert.deepEqual(parsed.own_stop_list, ["example.com/shorts", "news.site"]);
  });

  it("пределы числа записей защищают от раздутого хранилища", () => {
    const rules = Object.fromEntries(Array.from({ length: MAX_SITE_RULES + 50 }, (_, index) => [`site${index}.example`, "never"]));
    assert.equal(Object.keys(parseAutoHandoffSettings({ site_rules: rules }).site_rules).length, MAX_SITE_RULES);
    const entries = Array.from({ length: MAX_OWN_STOP_LIST_ENTRIES + 50 }, (_, index) => `site${index}.example`);
    assert.equal(parseAutoHandoffSettings({ own_stop_list: entries }).own_stop_list.length, MAX_OWN_STOP_LIST_ENTRIES);
  });
});

describe("ключ сайта и запись стоп-листа", () => {
  it("siteKeyOfHost: регистр, www., точка в конце; не хост — null", () => {
    assert.equal(siteKeyOfHost("WWW.Example.COM."), "example.com");
    for (const bad of ["", "  ", "a b", "host:80", "a/b", "."]) assert.equal(siteKeyOfHost(bad), null, bad);
  });

  it("normalizeStopListEntry: схема и www. уходят, путь без завершающего слэша, запрос и якорь — отказ", () => {
    assert.equal(normalizeStopListEntry(" HTTPS://www.YouTube.com/Shorts/ "), "youtube.com/shorts");
    assert.equal(normalizeStopListEntry("example.com"), "example.com");
    for (const bad of ["", "example.com/a?b=1", "example.com/a#x", "exa mple.com", `${"a".repeat(301)}.com`]) assert.equal(normalizeStopListEntry(bad), null, bad);
  });
});

describe("чтение, запись и включение охраны", () => {
  it("записанное читается обратно; чтение берёт ключ настройки", async () => {
    const { store, values } = memoryStore();
    const settings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true, min_duration_s: 120 };
    await writeAutoHandoffSettings(store, settings);
    assert.deepEqual(values.get(AUTO_HANDOFF_SETTINGS_KEY), settings);
    assert.deepEqual(await readAutoHandoffSettings(store), settings);
  });

  it("autoHandoffSettingsFromChange: наш ключ разбирается, чужой — undefined", () => {
    assert.equal(autoHandoffSettingsFromChange({ other: { newValue: 1 } }), undefined);
    assert.equal(autoHandoffSettingsFromChange({ [AUTO_HANDOFF_SETTINGS_KEY]: { newValue: { enabled: true } } })?.enabled, true);
    assert.deepEqual(autoHandoffSettingsFromChange({ [AUTO_HANDOFF_SETTINGS_KEY]: {} }), DEFAULT_AUTO_HANDOFF_SETTINGS);
  });

  it("сбой хранилища: значения по умолчанию (режим выключен) и причина в журнал", async () => {
    const notes: string[] = [];
    const broken: KeyValueStore = { get: () => Promise.reject(new Error("disk")), set: async () => {} };
    const settings = await readAutoHandoffSettingsOrDefaults(broken, (message) => notes.push(message));
    assert.deepEqual(settings, DEFAULT_AUTO_HANDOFF_SETTINGS);
    assert.equal(notes.length, 1);
  });

  it("wantsAutoHandoffWatch: включён или есть правило always/ask; одни never охраны не включают", () => {
    const base = DEFAULT_AUTO_HANDOFF_SETTINGS;
    assert.equal(wantsAutoHandoffWatch(base), false);
    assert.equal(wantsAutoHandoffWatch({ ...base, enabled: true }), true);
    assert.equal(wantsAutoHandoffWatch({ ...base, site_rules: { "a.com": "always" } }), true);
    assert.equal(wantsAutoHandoffWatch({ ...base, site_rules: { "a.com": "ask", "b.com": "never" } }), true);
    assert.equal(wantsAutoHandoffWatch({ ...base, site_rules: { "b.com": "never" } }), false);
  });
});
