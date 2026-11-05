// Чистые функции для фактов кандидата видео (09c): длительность и имя хоста файла.
// Без DOM и без глобалов страницы — проверяются тестами напрямую.

import { MAX_HOST_NAME_LENGTH } from "../shared/page-message-validation.ts";

/** Печатный ASCII без пробела — то же правило, что у договора (`isHostName`). */
const PRINTABLE_HOST_PATTERN = /^[\x21-\x7e]+$/;

/**
 * Длительность элемента для договора: конечное неотрицательное число секунд, иначе `null`.
 * `NaN` — метаданных ещё нет, `Infinity` — эфир без конца; фону обе ничего не говорят о фильме.
 */
export function finiteDurationSeconds(seconds: unknown): number | null {
  return typeof seconds === "number" && Number.isFinite(seconds) && seconds >= 0 ? seconds : null;
}

/** Размер кадра для договора: целое неотрицательное число, всё прочее (NaN, минус) — 0 «неизвестно». */
export function frameDimensionPx(pixels: number): number {
  return Number.isFinite(pixels) ? Math.max(0, Math.trunc(pixels)) : 0;
}

/**
 * Имя хоста абсолютного http(s)-адреса, разобранное вручную. Не `new URL`: конструктор `URL`
 * глобален в мире страницы, и она может подменить его так, чтобы факт о файле соврал. Адрес — это
 * `currentSrc` настоящего элемента, то есть уже канонический (схема и хост строчными, без пробелов).
 * `null` — это не http(s), имя пустое или слишком длинное для договора.
 */
export function hostOfHttpUrl(url: string): string | null {
  // Схема, затем «//», затем необязательные «логин:пароль@», затем хост (IPv6 — в скобках), затем порт/путь.
  const match = /^https?:\/\/(?:[^/?#@]*@)?(\[[^\]/?#]*\]|[^:/?#\[\]]*)/i.exec(url);
  const host = match?.[1]?.toLowerCase();
  if (host === undefined || host.length === 0 || host.length > MAX_HOST_NAME_LENGTH) return null;
  // Договор принимает только печатный ASCII без пробела; всё иное отдаём как «хоста нет», а не
  // как строку, из-за которой изолированный мир отверг бы весь ответ.
  return PRINTABLE_HOST_PATTERN.test(host) ? host : null;
}
