// Хранилища модели вкладки за швом `TabModelStore`: боевое — `storage.session`, запасное и
// тестовое — память.

/** Шов хранилища снимков. Значения — структурно-клонируемые данные (JSON-подобные). */
export interface TabModelStore {
  get(key: string): Promise<unknown>;
  set(key: string, value: unknown): Promise<void>;
  remove(key: string): Promise<void>;
  /** Все ключи хранилища (уборка записей после перезапуска фона). */
  keys(): Promise<readonly string[]>;
}

/** Нужная нам часть `chrome.storage.session` (и `StorageArea` вообще). */
export interface SessionStorageArea {
  /** `null` — все значения. Отдельного `getKeys` нет в Firefox, а записей немного: цена одна. */
  get(key: string | null): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
  remove(key: string): Promise<void>;
}

/**
 * Хранилище на `storage.session`: живёт, пока открыт браузер, переживает засыпание фона и стирается
 * при закрытии браузера (на диск не пишется).
 */
export function createSessionStorageTabModelStore(area: SessionStorageArea): TabModelStore {
  return {
    async get(key) {
      const items = await area.get(key);
      return items[key];
    },
    set: (key, value) => area.set({ [key]: value }),
    remove: (key) => area.remove(key),
    keys: async () => Object.keys(await area.get(null)),
  };
}

/** Хранилище в памяти: запасное, если у браузера нет `storage.session`, и тестовое. */
export function createMemoryTabModelStore(): TabModelStore {
  const items = new Map<string, unknown>();
  return {
    get: (key) => Promise.resolve(items.has(key) ? structuredClone(items.get(key)) : undefined),
    set(key, value) {
      items.set(key, structuredClone(value));
      return Promise.resolve();
    },
    remove(key) {
      items.delete(key);
      return Promise.resolve();
    },
    keys: () => Promise.resolve([...items.keys()]),
  };
}
