// Настройка набора входов (сессия 10c) в `storage.local`: какие входы включены. Общая для фона (меню,
// кнопка), контент-скрипта (кнопка поверх видео) и страницы настроек (запись). Устроена как
// `local-address-setting.ts`: хранилищу не доверяем слепо, мусор читается как значения по умолчанию.
import type { KeyValueStore } from "./codec-storage.ts";

/** Какие входы включены. Пункт меню на странице относится к `video_menu` (одно «правый клик по видео или странице»). */
export interface EntrySettings {
  /** Правый клик по видео и пункт на странице вокруг видео. */
  readonly video_menu: boolean;
  /** Правый клик по ссылке. */
  readonly link_menu: boolean;
  /** Кнопка поверх видео. Выключена по умолчанию (решение 53). */
  readonly overlay_button: boolean;
}

/** Имена полей настройки: порядок = порядок чекбоксов на странице настроек. */
export const ENTRY_SETTING_NAMES = ["video_menu", "link_menu", "overlay_button"] as const;
export type EntrySettingName = (typeof ENTRY_SETTING_NAMES)[number];

/** По умолчанию (решение 53): меню включены, кнопка поверх видео выключена. */
export const DEFAULT_ENTRY_SETTINGS: EntrySettings = { video_menu: true, link_menu: true, overlay_button: false };

/** Ключ настройки в `storage.local`. */
export const ENTRY_SETTINGS_KEY = "entrySettings";

/**
 * Разобрать то, что лежит в хранилище. Не объект — значения по умолчанию целиком; поле не булево или
 * отсутствует — значение по умолчанию только для этого поля (одно битое поле не сбрасывает остальные).
 */
export function parseEntrySettings(stored: unknown): EntrySettings {
  if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return DEFAULT_ENTRY_SETTINGS;
  const record = stored as Record<string, unknown>;
  const pick = (name: EntrySettingName): boolean => {
    const value = record[name];
    return typeof value === "boolean" ? value : DEFAULT_ENTRY_SETTINGS[name];
  };
  return { video_menu: pick("video_menu"), link_menu: pick("link_menu"), overlay_button: pick("overlay_button") };
}

export async function readEntrySettings(store: KeyValueStore): Promise<EntrySettings> {
  return parseEntrySettings((await store.get([ENTRY_SETTINGS_KEY]))[ENTRY_SETTINGS_KEY]);
}

export async function writeEntrySettings(store: KeyValueStore, settings: EntrySettings): Promise<void> {
  await store.set({ [ENTRY_SETTINGS_KEY]: settings });
}

/** Новое значение `storage.onChanged` для нашего ключа (`undefined` — наш ключ в изменениях не участвует). */
export function entrySettingsFromChange(changes: Readonly<Record<string, { readonly newValue?: unknown }>>): EntrySettings | undefined {
  const change = changes[ENTRY_SETTINGS_KEY];
  return change === undefined ? undefined : parseEntrySettings(change.newValue);
}

/**
 * Прочитать настройку для входа, который не должен молчать из-за сбоя хранилища: сбой — значения по умолчанию,
 * вход идёт дальше, причина уходит в `note` (журнал разработчика).
 */
export async function readEntrySettingsOrDefaults(store: KeyValueStore, note: (message: string, error: unknown) => void): Promise<EntrySettings> {
  try {
    return await readEntrySettings(store);
  } catch (error) {
    note("could not read the entry settings, using the defaults", error);
    return DEFAULT_ENTRY_SETTINGS;
  }
}
