// Настройка набора входов (сессия 10c): чтение с мусором, запись, значения по умолчанию, `storage.onChanged`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { KeyValueStore } from "../src/shared/codec-storage.ts";
import {
  DEFAULT_ENTRY_SETTINGS,
  ENTRY_SETTINGS_KEY,
  entrySettingsFromChange,
  parseEntrySettings,
  readEntrySettings,
  readEntrySettingsOrDefaults,
  writeEntrySettings,
} from "../src/shared/entry-settings.ts";

function memoryStore(initial: Record<string, unknown> = {}): KeyValueStore & { readonly data: Record<string, unknown> } {
  const data = { ...initial };
  return {
    data,
    get: (keys) => Promise.resolve(Object.fromEntries(keys.filter((key) => key in data).map((key) => [key, data[key]]))),
    set: (items) => {
      Object.assign(data, items);
      return Promise.resolve();
    },
  };
}

describe("настройка входов", () => {
  it("по умолчанию меню включены, кнопка поверх видео выключена (решение 53)", async () => {
    assert.deepEqual(await readEntrySettings(memoryStore()), { video_menu: true, link_menu: true, overlay_button: false });
  });

  it("записанное читается обратно", async () => {
    const store = memoryStore();
    await writeEntrySettings(store, { video_menu: false, link_menu: true, overlay_button: true });
    assert.deepEqual(await readEntrySettings(store), { video_menu: false, link_menu: true, overlay_button: true });
  });

  it("мусор вместо объекта читается как значения по умолчанию", () => {
    for (const junk of ["yes", 7, null, ["video_menu"], undefined, true]) {
      assert.deepEqual(parseEntrySettings(junk), DEFAULT_ENTRY_SETTINGS, String(junk));
    }
  });

  it("одно битое поле сбрасывается только само, остальные сохраняются", () => {
    assert.deepEqual(parseEntrySettings({ video_menu: false, link_menu: "no", overlay_button: 1 }), { video_menu: false, link_menu: true, overlay_button: false });
    assert.deepEqual(parseEntrySettings({ overlay_button: true, unknown_field: true }), { video_menu: true, link_menu: true, overlay_button: true });
  });

  it("запись не трогает чужие ключи хранилища", async () => {
    const store = memoryStore({ codecSetting: "auto" });
    await writeEntrySettings(store, DEFAULT_ENTRY_SETTINGS);
    assert.equal(store.data["codecSetting"], "auto");
  });

  it("onChanged: новое значение нашего ключа разбирается, чужой ключ — undefined, удаление ключа — по умолчанию", () => {
    assert.deepEqual(entrySettingsFromChange({ [ENTRY_SETTINGS_KEY]: { newValue: { video_menu: false, link_menu: false, overlay_button: true } } }), {
      video_menu: false,
      link_menu: false,
      overlay_button: true,
    });
    assert.equal(entrySettingsFromChange({ codecSetting: { newValue: "off" } }), undefined);
    assert.deepEqual(entrySettingsFromChange({ [ENTRY_SETTINGS_KEY]: {} }), DEFAULT_ENTRY_SETTINGS);
  });

  it("сбой чтения хранилища: значения по умолчанию, вход идёт дальше, причина в журнал (не молчит)", async () => {
    const notes: string[] = [];
    const broken: KeyValueStore = { get: () => Promise.reject(new Error("storage unavailable")), set: () => Promise.resolve() };
    assert.deepEqual(await readEntrySettingsOrDefaults(broken, (message) => void notes.push(message)), DEFAULT_ENTRY_SETTINGS);
    assert.equal(notes.length, 1);
    const fine = memoryStore({ [ENTRY_SETTINGS_KEY]: { video_menu: false, link_menu: true, overlay_button: true } });
    assert.equal((await readEntrySettingsOrDefaults(fine, () => assert.fail("не должно быть сбоя"))).video_menu, false);
  });
});
