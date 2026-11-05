// Настройка «Local network videos from internet pages» (решение 199) в `storage.local`. Общая для фона
// (проверка адреса файла) и страницы настроек (запись). Живёт в экстеншене, а не в `config.toml`:
// адресов плеер не знает (инвариант 2). Устроена как `codec-storage.ts`: хранилищу не доверяем слепо,
// незнакомое значение читается как значение по умолчанию.
import type { KeyValueStore } from "./codec-storage.ts";

/** `block` — локальный адрес со страницы из интернета запрещён; `ask` — решает человек в попапе 10. */
export type LocalAddressSetting = "block" | "ask";

export const LOCAL_ADDRESS_SETTINGS: readonly LocalAddressSetting[] = ["block", "ask"];

/** По умолчанию — блокировать (решение 199). */
export const DEFAULT_LOCAL_ADDRESS_SETTING: LocalAddressSetting = "block";

/** Ключ настройки в `storage.local`. */
export const LOCAL_ADDRESS_SETTING_KEY = "localAddressSetting";

export async function readLocalAddressSetting(store: KeyValueStore): Promise<LocalAddressSetting> {
  const stored = (await store.get([LOCAL_ADDRESS_SETTING_KEY]))[LOCAL_ADDRESS_SETTING_KEY];
  return LOCAL_ADDRESS_SETTINGS.find((setting) => setting === stored) ?? DEFAULT_LOCAL_ADDRESS_SETTING;
}

export async function writeLocalAddressSetting(store: KeyValueStore, setting: LocalAddressSetting): Promise<void> {
  await store.set({ [LOCAL_ADDRESS_SETTING_KEY]: setting });
}
