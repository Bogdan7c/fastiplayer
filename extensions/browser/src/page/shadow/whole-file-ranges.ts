// `buffered` и `seekable` тени файла: «весь файл» `[0, длительность]` (решение владельца 187).
//
// Почему весь файл: настоящий элемент файл больше не качает (источник снят, решение 186), а
// плеер страницы по `buffered` решает, ждать ли данных. Пустой `buffered` показал бы ему вечную
// буферизацию, а «весь файл» честно отвечает на его вопрос «можно ли играть дальше» — байты
// любого места плеер Fastiplayer получит сам по `fetch_range`. Протокол не меняется.
//
// Проверка «бренда»: страница может проверять `instanceof TimeRanges` или звать
// `TimeRanges.prototype.end.call(ranges, 0)`. Поэтому:
// - если настоящий `seekable` файла, снятый ДО снятия источника, — ровно один промежуток
//   `[0, длительность]` (обычный файл с поддержкой `Range`), отдаём его: это НАСТОЯЩИЙ объект
//   TimeRanges браузера, все проверки он проходит. Цена — один и тот же объект при каждом чтении
//   (браузер каждый раз создаёт новый); на это плееры не смотрят;
// - иначе — свой объект с прототипом TimeRanges (`instanceof` и `toString` проходят), собственными
//   `length`/`start`/`end` с ошибками браузера (IndexSizeError, TypeError без аргумента). Вызов
//   оригинала `TimeRanges.prototype.end.call(...)` на нём бросит — это известное ограничение.

import type { ElementOriginals } from "./originals.ts";
import { readOriginal } from "./originals.ts";

/** Та часть TimeRanges, которой пользуются плееры. */
export interface TimeRangesShape {
  readonly length: number;
  start(index: number): number;
  end(index: number): number;
}

/**
 * Длительность файла для «весь файл»: конечная неотрицательная — один промежуток; иначе (метаданных
 * не было, NaN) — пустые промежутки, как у браузера до загрузки метаданных.
 */
function wholeFileSpan(durationSeconds: number): readonly [number, number] | undefined {
  return Number.isFinite(durationSeconds) && durationSeconds > 0 ? [0, durationSeconds] : undefined;
}

/** Настоящий `seekable` — ровно `[0, длительность]`: годится как «весь файл». Чтения могут бросить. */
function isWholeFile(ranges: unknown, span: readonly [number, number]): ranges is TimeRangesShape {
  if (typeof ranges !== "object" || ranges === null) return false;
  const shape = ranges as TimeRangesShape;
  return shape.length === 1 && shape.start(0) === span[0] && shape.end(0) === span[1];
}

/**
 * Снять «весь файл» с настоящего элемента ДО снятия источника. Возвращает фабрику: каждое чтение
 * `buffered`/`seekable` страницей получает объект из неё.
 */
export function captureWholeFileRanges(element: HTMLMediaElement, originals: ElementOriginals, durationSeconds: number): () => TimeRangesShape {
  const span = wholeFileSpan(durationSeconds);
  const seekable = originals.media.seekable;
  if (span !== undefined && seekable !== undefined) {
    try {
      const real = readOriginal(seekable, element);
      if (isWholeFile(real, span)) return () => real;
    } catch {
      // Чужой «бренд» или старый браузер — тогда свой объект ниже.
    }
  }
  return () => createTimeRanges(span, originals);
}

/** Свой TimeRanges из 0 или 1 промежутка, с ошибками как у браузера. */
export function createTimeRanges(span: readonly [number, number] | undefined, originals: ElementOriginals): TimeRangesShape {
  const ranges: readonly (readonly [number, number])[] = span === undefined ? [] : [span];
  const boundary = (which: 0 | 1, operation: "start" | "end") =>
    function (this: unknown, ...args: unknown[]): number {
      if (args.length === 0) {
        throw new originals.TypeErrorConstructor(`Failed to execute '${operation}' on 'TimeRanges': 1 argument required, but only 0 present.`);
      }
      // WebIDL `unsigned long`: число по модулю 2^32 (NaN → 0).
      const index = Number(args[0]) >>> 0;
      const range = ranges[index];
      if (range === undefined) {
        throw new originals.DOMExceptionConstructor(
          `Failed to execute '${operation}' on 'TimeRanges': The index provided (${index}) is greater than or equal to the maximum bound (${ranges.length}).`,
          "IndexSizeError",
        );
      }
      return range[which];
    };
  const prototype = originals.timeRangesPrototype ?? Object.prototype;
  const shape = Object.create(prototype) as object;
  Object.defineProperties(shape, {
    length: { value: ranges.length, enumerable: true },
    start: { value: boundary(0, "start") },
    end: { value: boundary(1, "end") },
  });
  return shape as TimeRangesShape;
}
