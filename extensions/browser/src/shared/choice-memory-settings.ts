// Настройка памяти выбора (сессия 14b2, решение 281) в `storage.local`: сколько сериалов помнить.
//
// Общая для фона (вытеснение при записи) и страницы настроек (запись). Устроена как `entry-settings.ts`:
// хранилищу не доверяем слепо, мусор и число вне диапазона читаются как значение по умолчанию.
import type { KeyValueStore } from "./codec-storage.ts";

/** Настройка памяти выбора. */
export interface ChoiceMemorySettings {
  /** Сколько сериалов помнить; при переполнении вытесняются те, которых дольше всего не касались. */
  readonly capacity: number;
}

/** По умолчанию — 500 сериалов (решение 281). */
export const DEFAULT_CHOICE_MEMORY_CAPACITY = 500;

/**
 * Разумный диапазон объёма. Нижняя граница — чтобы сериалы, которые смотрят по очереди, не выталкивали
 * друг друга; верхняя — чтобы одна запись `storage.local` с памятью оставалась маленькой (сотни килобайт).
 */
export const MIN_CHOICE_MEMORY_CAPACITY = 10;
export const MAX_CHOICE_MEMORY_CAPACITY = 5_000;

export const DEFAULT_CHOICE_MEMORY_SETTINGS: ChoiceMemorySettings = { capacity: DEFAULT_CHOICE_MEMORY_CAPACITY };

/** Ключ настройки в `storage.local`. */
export const CHOICE_MEMORY_SETTINGS_KEY = "choiceMemorySettings";

/** Годится ли число как объём памяти: целое в разумном диапазоне. */
export function isValidChoiceMemoryCapacity(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= MIN_CHOICE_MEMORY_CAPACITY && value <= MAX_CHOICE_MEMORY_CAPACITY;
}

/** Разобрать то, что лежит в хранилище: не объект или негодное число — значение по умолчанию. */
export function parseChoiceMemorySettings(stored: unknown): ChoiceMemorySettings {
  if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return DEFAULT_CHOICE_MEMORY_SETTINGS;
  const capacity = (stored as Record<string, unknown>)["capacity"];
  return isValidChoiceMemoryCapacity(capacity) ? { capacity } : DEFAULT_CHOICE_MEMORY_SETTINGS;
}

export async function readChoiceMemorySettings(store: KeyValueStore): Promise<ChoiceMemorySettings> {
  return parseChoiceMemorySettings((await store.get([CHOICE_MEMORY_SETTINGS_KEY]))[CHOICE_MEMORY_SETTINGS_KEY]);
}

export async function writeChoiceMemorySettings(store: KeyValueStore, settings: ChoiceMemorySettings): Promise<void> {
  await store.set({ [CHOICE_MEMORY_SETTINGS_KEY]: settings });
}

/**
 * Разобрать то, что человек ввёл в поле объёма: целое число в диапазоне или `undefined` (страница настроек
 * покажет подсказку и ничего не запишет).
 */
export function parseChoiceMemoryCapacityInput(text: string): number | undefined {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return undefined;
  const capacity = Number(trimmed);
  return isValidChoiceMemoryCapacity(capacity) ? capacity : undefined;
}
