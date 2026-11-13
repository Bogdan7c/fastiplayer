// Перевод значений MSE страницы в значения протокола: время, длительность, MIME.
//
// Время в протоколе — целые микросекунды (time.ts): MSE отдаёт секунды дробным числом, мы
// округляем до ближайшей микросекунды. Значения вне диапазона протокола обрезаются до границы, а
// нечисла — не пересказываются (вызывающий решает, что делать с `null`).

import { isEndlessDurationSeconds } from "../shared/endless-duration.ts";
import { MAX_JSON_SAFE_INTEGER, MAX_MIME_TYPE_BYTES } from "../protocol/limits.ts";
import type { MediaDuration, MediaTimeMicros, MediaTimeOffsetMicros } from "../protocol/time.ts";
import type { RemovalEnd } from "../protocol/session/page.ts";

/** Микросекунд в секунде. */
const MICROS_PER_SECOND = 1_000_000;

/** Секунды → целые микросекунды в границах ±(2^53 − 1); `null` — не конечное число. */
function roundToMicros(seconds: number): number | null {
  if (!Number.isFinite(seconds)) return null;
  const micros = Math.round(seconds * MICROS_PER_SECOND);
  return Math.max(-MAX_JSON_SAFE_INTEGER, Math.min(MAX_JSON_SAFE_INTEGER, micros));
}

/** Позиция (неотрицательная): `null` — не конечное число. */
export function positionMicros(seconds: unknown): MediaTimeMicros | null {
  if (typeof seconds !== "number") return null;
  const micros = roundToMicros(seconds);
  return micros === null ? null : Math.max(0, micros);
}

/** `timestampOffset` (может быть отрицательным): `null` — не конечное число. */
export function offsetMicros(seconds: unknown): MediaTimeOffsetMicros | null {
  if (typeof seconds !== "number") return null;
  return roundToMicros(seconds);
}

/**
 * `MediaSource.duration`: NaN — неизвестна, +∞ или длиннее года — живой поток (решение 319,
 * `shared/endless-duration.ts`), иначе конечная.
 */
export function durationOf(seconds: number): MediaDuration {
  if (Number.isNaN(seconds)) return { kind: "unknown" };
  if (isEndlessDurationSeconds(seconds)) return { kind: "live" };
  return { kind: "finite", duration_us: positionMicros(seconds) ?? 0 };
}

/** Конец `remove(start, end)`: `Infinity` — «до конца таймлайна». */
export function removalEndOf(endSeconds: unknown): RemovalEnd | null {
  if (endSeconds === Number.POSITIVE_INFINITY) return { kind: "end_of_timeline" };
  const micros = positionMicros(endSeconds);
  return micros === null ? null : { kind: "at", time_us: micros };
}

/**
 * Аргумент, как его увидел браузер. Хук зовётся ПОСЛЕ успешного оригинала, а браузер приводит
 * аргументы по WebIDL: `remove("5", "10")` — числа 5 и 10, `changeType(new String(mime))` — строка.
 * Мы приводим так же (`Number`/`String`), чтобы пересказ совпал с тем, что сделал браузер.
 * Для объекта это второй вызов его `valueOf`/`toString` (первый сделал браузер) — так же поступает
 * любой полифил; примитивы приводятся без чужого кода.
 */
export function asWebIdlDouble(value: unknown): number {
  return typeof value === "number" ? value : Number(value);
}

export function asWebIdlString(value: unknown): string {
  return typeof value === "string" ? value : String(value);
}

/** Одинаковы ли две длительности (для «изменилась ли длительность после endOfStream»). */
export function sameDuration(left: MediaDuration, right: MediaDuration): boolean {
  if (left.kind !== right.kind) return false;
  return left.kind !== "finite" || (right.kind === "finite" && left.duration_us === right.duration_us);
}

/** Печатный ASCII (U+0020–U+007E): так MIME записан в протоколе (text.ts). */
const PRINTABLE_ASCII = /^[\x20-\x7e]+$/;

/**
 * Годится ли MIME для протокола: печатный ASCII, 1..256 байт (у ASCII байт = символ).
 * Иначе SourceBuffer не отслеживается: плеер такое сообщение всё равно отверг бы.
 */
export function isProtocolMimeType(mimeType: unknown): mimeType is string {
  return typeof mimeType === "string" && mimeType.length >= 1 && mimeType.length <= MAX_MIME_TYPE_BYTES && PRINTABLE_ASCII.test(mimeType);
}
