// Общие заготовки тестов окон настройки автозапуска (попап 12b и страница настроек 12b): минимальный
// фейковый DOM, хранилище в памяти и «шина» onChanged.
import assert from "node:assert/strict";
import { AUTO_HANDOFF_SETTINGS_KEY, parseAutoHandoffSettings, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import type { KeyValueStore } from "../../src/shared/codec-storage.ts";

/** Минимальный элемент: ровно то, что трогают контролы. */
export class FakeElement {
  id = "";
  value = "";
  checked = false;
  min = "";
  max = "";
  type = "";
  className = "";
  placeholder = "";
  textContent = "";
  children: FakeElement[] = [];
  readonly listeners = new Map<string, () => void>();
  readonly tag: string;
  constructor(tag: string) {
    this.tag = tag;
  }
  append(...nodes: FakeElement[]): void {
    this.children.push(...nodes);
  }
  replaceChildren(...nodes: FakeElement[]): void {
    this.children = [...nodes];
  }
  addEventListener(type: string, listener: () => void): void {
    this.listeners.set(type, listener);
  }
  fire(type: string): void {
    this.listeners.get(type)?.();
  }
  all(): FakeElement[] {
    return this.children.flatMap((child) => [child, ...child.all()]);
  }
  /** Элемент по id; нет — тест падает. */
  byId(id: string): FakeElement {
    const found = this.all().find((element) => element.id === id);
    assert.ok(found, `нет элемента #${id}`);
    return found;
  }
  /** Все элементы с тегом и классом (класс пустой — любые). */
  select(tag: string, className = ""): FakeElement[] {
    return this.all().filter((element) => element.tag === tag && (className === "" || element.className === className));
  }
  /** Единственный элемент с тегом и классом. */
  one(tag: string, className = ""): FakeElement {
    const found = this.select(tag, className);
    assert.equal(found.length, 1, `${tag}.${className}`);
    return found[0] as FakeElement;
  }
  /** Тексты потомков с тегом (для проверки списков). */
  texts(tag: string): string[] {
    return this.select(tag).map((element) => element.textContent);
  }
}

/** Подменить `document.createElement` на время действия. */
export async function withFakeDocument<T>(action: () => Promise<T>): Promise<T> {
  const globals = globalThis as unknown as { document?: unknown };
  const saved = globals.document;
  globals.document = { createElement: (tag: string) => new FakeElement(tag) };
  try {
    return await action();
  } finally {
    globals.document = saved;
  }
}

/** Хранилище в памяти с учётом записей и возможностью сломать чтение или запись. */
export function memoryStore(initial: Record<string, unknown> = {}) {
  const values = new Map(Object.entries(initial));
  const state = { writes: 0, failReads: false, failWrites: false };
  const store: KeyValueStore = {
    get: async (keys) => {
      if (state.failReads) throw new Error("read broke");
      return Object.fromEntries(keys.filter((key) => values.has(key)).map((key) => [key, values.get(key)]));
    },
    set: async (items) => {
      if (state.failWrites) throw new Error("write broke");
      state.writes += 1;
      for (const [key, value] of Object.entries(items)) values.set(key, value);
    },
  };
  return { store, values, state };
}

/** Что сейчас записано в настройке (разобрано так же, как читает код). */
export function storedSettings(values: Map<string, unknown>): AutoHandoffSettings {
  return parseAutoHandoffSettings(values.get(AUTO_HANDOFF_SETTINGS_KEY));
}

/** «Шина» `onSettingsChanged`: `emit` вызывает всех подписчиков, как это делал бы `storage.onChanged`. */
export function settingsChangeBus() {
  const listeners: ((settings: AutoHandoffSettings) => void)[] = [];
  return {
    subscribe: (listener: (settings: AutoHandoffSettings) => void): void => void listeners.push(listener),
    emit: (settings: AutoHandoffSettings): void => listeners.forEach((listener) => listener(settings)),
  };
}
