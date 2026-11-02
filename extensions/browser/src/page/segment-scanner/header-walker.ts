// Общий автомат сканеров: «дочитать заголовок → перепрыгнуть тело» поверх кусков любой нарезки.
//
// Формат (MP4 или WebM) решает только одно: когда заголовок полон и что он значит. Всё остальное —
// счёт смещений, перепрыгивание тел, сшивка заголовка, разрезанного между кусками, — здесь.

import type { ScanOutcome, SegmentMarker, SegmentMarkerKind, SegmentScanner } from "./scanner.ts";

/**
 * Самый длинный заголовок, который бывает: MP4 с `largesize` — 16 байт; WebM — ID до 4 байт и
 * размер до 8 байт, итого 12. Больше заголовок быть не может — это признак мусора.
 */
export const MAX_HEADER_BYTES = 16;

/** Что формат решил по накопленному заголовку. */
export type HeaderStep =
  /** Заголовок ещё не полон — нужен следующий байт. */
  | { readonly kind: "need_more" }
  /**
   * Заголовок полон: перепрыгнуть `bodyBytes` байт тела. `markerAtBodyEnd` — метка, которая
   * встаёт сразу после тела (например, конец init-сегмента после `moov`).
   */
  | { readonly kind: "skip_body"; readonly bodyBytes: number; readonly markerAtBodyEnd: SegmentMarkerKind | null }
  | { readonly kind: "failed"; readonly reason: string };

/** Недочитанный заголовок: до `MAX_HEADER_BYTES` байт (копируются только они). */
export class HeaderBytes {
  readonly #bytes = new Uint8Array(MAX_HEADER_BYTES);
  #length = 0;

  get length(): number {
    return this.#length;
  }

  /** Байт заголовка по номеру (вызывающий проверяет, что номер < length). */
  byteAt(index: number): number {
    return this.#bytes[index] ?? 0;
  }

  /** Целое без знака из `count` байт с позиции `start` (старший байт первым). */
  readUnsigned(start: number, count: number): number {
    let value = 0;
    for (let index = start; index < start + count; index += 1) {
      value = value * 256 + this.byteAt(index);
    }
    return value;
  }

  push(byte: number): boolean {
    if (this.#length >= MAX_HEADER_BYTES) return false;
    this.#bytes[this.#length] = byte;
    this.#length += 1;
    return true;
  }

  clear(): void {
    this.#length = 0;
  }
}

/** База сканеров: подкласс реализует только разбор полного заголовка. */
export abstract class HeaderWalker implements SegmentScanner {
  protected readonly header = new HeaderBytes();
  /** Смещение следующего байта от начала потока SourceBuffer. */
  #position: number;
  /** Сколько байтов тела текущего бокса/элемента ещё перепрыгнуть. */
  #bodyBytesLeft = 0;
  #markerAtBodyEnd: SegmentMarkerKind | null = null;
  /** Где начался текущий (недочитанный) заголовок. */
  #headerStart = 0;
  #failure: string | null = null;

  protected constructor(streamOffset: number) {
    this.#position = streamOffset;
  }

  /**
   * Заголовок пополнился одним байтом. `headerStart` — смещение его начала в потоке; метки
   * начала (init_start, media_start) подкласс кладёт в `markers` сам.
   */
  protected abstract onHeaderByte(headerStart: number, markers: SegmentMarker[]): HeaderStep;

  /** Сброс состояния формата при `abort()` (недочитанный заголовок база уже забыла). */
  protected abstract onRestart(): void;

  scan(chunk: Uint8Array): ScanOutcome {
    if (this.#failure !== null) return { kind: "failed", reason: this.#failure };
    const markers: SegmentMarker[] = [];
    let index = 0;
    while (index < chunk.length) {
      if (this.#bodyBytesLeft > 0) {
        // Тело перепрыгиваем целиком, не читая ни байта.
        const step = Math.min(this.#bodyBytesLeft, chunk.length - index);
        this.#bodyBytesLeft -= step;
        index += step;
        this.#position += step;
        if (this.#bodyBytesLeft === 0) this.#finishBody(markers);
        continue;
      }
      if (this.header.length === 0) this.#headerStart = this.#position;
      if (!this.header.push(chunk[index] ?? 0)) return this.#fail("header longer than any valid header");
      index += 1;
      this.#position += 1;
      const step = this.onHeaderByte(this.#headerStart, markers);
      if (step.kind === "need_more") continue;
      if (step.kind === "failed") return this.#fail(step.reason);
      this.header.clear();
      this.#bodyBytesLeft = step.bodyBytes;
      this.#markerAtBodyEnd = step.markerAtBodyEnd;
      if (this.#bodyBytesLeft === 0) this.#finishBody(markers);
    }
    return { kind: "scanned", markers };
  }

  restartAtNextAppend(): void {
    if (this.#failure !== null) return;
    this.header.clear();
    this.#bodyBytesLeft = 0;
    this.#markerAtBodyEnd = null;
    this.onRestart();
  }

  #finishBody(markers: SegmentMarker[]): void {
    if (this.#markerAtBodyEnd !== null) markers.push({ kind: this.#markerAtBodyEnd, offset: this.#position });
    this.#markerAtBodyEnd = null;
  }

  #fail(reason: string): ScanOutcome {
    this.#failure = reason;
    return { kind: "failed", reason };
  }
}
