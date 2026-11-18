// Настройка «сколько ждать видео после перезагрузки страницы» (сессия 16c, решение 305; умолчание — сессия 16e2,
// решение 343) в `storage.local`.
//
// Общая для фона (сеанс передачи читает её при открытии) и страницы настроек (запись). Устроена как
// `choice-memory-settings.ts`: хранилищу не доверяем слепо, мусор и число вне диапазона читаются как значение
// по умолчанию. Хранятся целые секунды — так их вводит человек; фону отдаются миллисекунды.
import type { KeyValueStore } from "./codec-storage.ts";

/** Настройка ожидания после перезагрузки. */
export interface PageReloadWaitSettings {
  /** Сколько секунд окно ждёт видео нового документа той же вкладки; `0` — не ждать (конец сразу, как до 16c). */
  readonly seconds: number;
}

/**
 * По умолчанию — 30 с (решение 343 вместо 10 с решения 305). На сайтах, где видео новой страницы само не стартует
 * (uaserials: плеер во фрейме, запуск кликом), человек долистывает до плеера и жмёт play — живьём это 16-я секунда,
 * а 10 с уже вышли. Долгий срок почти незаметен: пока он идёт, окно досматривает скачанное (решение 315).
 * Цена одна — после ухода на страницу без видео «Source tab closed» приходит позже.
 * Уже сохранённое значение не трогаем: нет записи в хранилище — значит, действует это умолчание.
 */
export const DEFAULT_PAGE_RELOAD_WAIT_SECONDS = 30;

/** `0` выключает ожидание; дольше минуты окно с последним кадром выглядело бы зависшим. */
export const MIN_PAGE_RELOAD_WAIT_SECONDS = 0;
export const MAX_PAGE_RELOAD_WAIT_SECONDS = 60;

export const DEFAULT_PAGE_RELOAD_WAIT_SETTINGS: PageReloadWaitSettings = { seconds: DEFAULT_PAGE_RELOAD_WAIT_SECONDS };

/** Ключ настройки в `storage.local`. */
export const PAGE_RELOAD_WAIT_SETTINGS_KEY = "pageReloadWaitSettings";

const MS_PER_SECOND = 1_000;

/** Годится ли число как срок: целые секунды в диапазоне. */
export function isValidPageReloadWaitSeconds(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= MIN_PAGE_RELOAD_WAIT_SECONDS && value <= MAX_PAGE_RELOAD_WAIT_SECONDS;
}

/** Разобрать то, что лежит в хранилище: не объект или негодное число — значение по умолчанию. */
export function parsePageReloadWaitSettings(stored: unknown): PageReloadWaitSettings {
  if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return DEFAULT_PAGE_RELOAD_WAIT_SETTINGS;
  const seconds = (stored as Record<string, unknown>)["seconds"];
  return isValidPageReloadWaitSeconds(seconds) ? { seconds } : DEFAULT_PAGE_RELOAD_WAIT_SETTINGS;
}

export async function readPageReloadWaitSettings(store: KeyValueStore): Promise<PageReloadWaitSettings> {
  return parsePageReloadWaitSettings((await store.get([PAGE_RELOAD_WAIT_SETTINGS_KEY]))[PAGE_RELOAD_WAIT_SETTINGS_KEY]);
}

export async function writePageReloadWaitSettings(store: KeyValueStore, settings: PageReloadWaitSettings): Promise<void> {
  await store.set({ [PAGE_RELOAD_WAIT_SETTINGS_KEY]: settings });
}

/** Срок в миллисекундах для таймеров фона. */
export function pageReloadWaitMsOf(settings: PageReloadWaitSettings): number {
  return settings.seconds * MS_PER_SECOND;
}

/** Срок по умолчанию в миллисекундах: им живёт сеанс, если настройку прочитать не вышло. */
export const DEFAULT_PAGE_RELOAD_WAIT_MS = pageReloadWaitMsOf(DEFAULT_PAGE_RELOAD_WAIT_SETTINGS);

/**
 * Разобрать то, что человек ввёл в поле: целые секунды в диапазоне или `undefined` (страница настроек покажет
 * подсказку и ничего не запишет).
 */
export function parsePageReloadWaitInput(text: string): number | undefined {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return undefined;
  const seconds = Number(trimmed);
  return isValidPageReloadWaitSeconds(seconds) ? seconds : undefined;
}
