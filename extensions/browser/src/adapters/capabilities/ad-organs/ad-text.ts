// Разбор чисел из текста страницы. Текст страницы враждебен: берём из него только целые числа и
// сразу проверяем по лимитам протокола. Всё, что не лезет в лимиты, — «не знаем», а не ошибка.

import { MAX_AD_COUNTDOWN_SECONDS, MAX_ADS_IN_POD, type AdPod, type AdSkip } from "../../../protocol/index.ts";

/** Самый длинный текст-счётчик, который мы разбираем: длиннее — это уже не счётчик. */
const MAX_COUNTER_TEXT_LENGTH = 48;

/** Самое длинное число в цифрах: длиннее заведомо за лимитами протокола, а `Number` не теряет точность. */
const MAX_DIGITS_PER_NUMBER = 6;

/** Пробелы подряд (включая неразрывные) → один пробел, края обрезаны. */
export function normalizeText(text: string): string {
  return text.replace(/\s+/gu, " ").trim();
}

/**
 * Все целые числа текста по порядку. `undefined` — текст слишком длинный или число не влезает:
 * разбирать такое нельзя.
 */
export function integersIn(text: string): number[] | undefined {
  const normalized = normalizeText(text);
  if (normalized.length === 0 || normalized.length > MAX_COUNTER_TEXT_LENGTH) return undefined;
  const found = normalized.match(/\d+/gu) ?? [];
  if (found.some((digits) => digits.length > MAX_DIGITS_PER_NUMBER)) return undefined;
  return found.map(Number);
}

/** Номер рекламы в блоке по двум числам; вне лимитов или номер больше размера — `unknown`. */
export function adPodFromNumbers(index: number | undefined, count: number | undefined): AdPod {
  if (index === undefined || count === undefined) return { kind: "unknown" };
  const inLimits = index >= 1 && count >= 1 && index <= MAX_ADS_IN_POD && count <= MAX_ADS_IN_POD;
  return inLimits && index <= count ? { kind: "known", index, count } : { kind: "unknown" };
}

/** «N из M» в любом языке: ровно два числа, остальное игнорируется. Иное число чисел — `unknown`. */
export function adPodFromCounterText(text: string): AdPod {
  const numbers = integersIn(text);
  return numbers?.length === 2 ? adPodFromNumbers(numbers[0], numbers[1]) : { kind: "unknown" };
}

/** Отсчёт до пропуска по числу секунд; вне лимитов протокола — `unavailable`. */
export function countdownFromSeconds(seconds: number | undefined): AdSkip {
  return seconds !== undefined && seconds >= 0 && seconds <= MAX_AD_COUNTDOWN_SECONDS ? { kind: "countdown", seconds } : { kind: "unavailable" };
}

/** Отсчёт из текста: ровно одно число — секунды, иначе `unavailable`. */
export function countdownFromText(text: string): AdSkip {
  const numbers = integersIn(text);
  return numbers?.length === 1 ? countdownFromSeconds(numbers[0]) : { kind: "unavailable" };
}

/** Хост совпадает с суффиксом целиком или как поддомен (`www.youtube.com` ← `youtube.com`, но не `notyoutube.com`). */
export function hostMatches(hostname: string, suffixes: readonly string[]): boolean {
  const host = hostname.toLowerCase();
  return suffixes.some((suffix) => host === suffix || host.endsWith(`.${suffix}`));
}
