// Сканер границ WebM/Matroska (EBML): только заголовки элементов верхних уровней.
//
// Заголовок элемента: ID (1–4 байта, длина — по числу ведущих нулей первого байта) + размер
// (переменной длины 1–8 байт; все биты значения — единицы = «размер неизвестен»).
//
// Устройство потока MSE: EBML-заголовок → `Segment` (размер известен или неизвестен) → внутри
// элементы уровня 1 (`SeekHead`, `Info`, `Tracks`, `Cues`, `Tags`, …, `Cluster`).
// - Init-сегмент: от EBML-заголовка до начала первого `Cluster`.
// - Граница медиасегмента: начало каждого `Cluster` (ID 1F43B675).
// - `Segment` не перепрыгиваем, а «входим» в него. Его конец не отслеживаем: страница может класть
//   кластеры из разных мест файла (перемотка, повторная докачка), и счёт байтов от размера
//   `Segment` разошёлся бы с потоком. Новый EBML-заголовок в потоке — новый init (смена качества).
// - `Cluster` неизвестного размера (живые потоки) кончается там, где начинается следующий элемент
//   уровня `Segment` или верхнего уровня. Отдельного режима для него не нужно: в `Cluster` мы тоже
//   «входим», а его детей (`Timestamp`, `SimpleBlock`, `BlockGroup`, …) перепрыгиваем по их
//   размерам так же, как любой незнакомый элемент уровня 1, — следующий `Cluster` узнаётся по ID.
//   Ребёнок неизвестного размера — сбой (по спецификации такого не бывает).

import { HeaderWalker, type HeaderStep } from "./header-walker.ts";
import type { SegmentMarker } from "./scanner.ts";

/** ID элементов EBML/Matroska, которые сканеру нужно узнавать. */
const EBML_HEADER_ID = 0x1a45dfa3;
const SEGMENT_ID = 0x18538067;
const CLUSTER_ID = 0x1f43b675;

/** Самая длинная запись ID в EBML — 4 байта. */
const MAX_ID_BYTES = 4;
/** Самая длинная запись размера в EBML — 8 байт. */
const MAX_SIZE_BYTES = 8;

/** Где сейчас стоит разбор. */
type WebmLevel =
  /** До `Segment`: ждём EBML-заголовок и `Segment`. */
  | "top"
  /** Внутри `Segment`: элементы уровня 1 (и дети кластеров неизвестного размера). */
  | "segment";

/** Длина записи переменной длины по первому байту (число ведущих нулей + 1); 0 — запись неверна. */
function variableLengthOf(firstByte: number, maxBytes: number): number {
  for (let length = 1; length <= maxBytes; length += 1) {
    if (firstByte & (0x80 >> (length - 1))) return length;
  }
  return 0;
}

export class WebmScanner extends HeaderWalker {
  #level: WebmLevel = "top";
  /** EBML-заголовок встречен, первого `Cluster` после него ещё не было. */
  #initOpen = false;

  constructor(streamOffset: number) {
    super(streamOffset);
  }

  protected onHeaderByte(headerStart: number, markers: SegmentMarker[]): HeaderStep {
    const idLength = variableLengthOf(this.header.byteAt(0), MAX_ID_BYTES);
    if (idLength === 0) return { kind: "failed", reason: "ebml element id is invalid" };
    if (this.header.length <= idLength) return { kind: "need_more" };
    const sizeFirstByte = this.header.byteAt(idLength);
    const sizeLength = variableLengthOf(sizeFirstByte, MAX_SIZE_BYTES);
    if (sizeLength === 0) return { kind: "failed", reason: "ebml element size is invalid" };
    if (this.header.length < idLength + sizeLength) return { kind: "need_more" };

    const elementId = this.header.readUnsigned(0, idLength);
    const size = this.#readSize(idLength, sizeLength);
    return this.#dispatch(elementId, size, headerStart, markers);
  }

  protected onRestart(): void {
    // После abort() поток продолжается с нового элемента того же уровня (внутри Segment — обычно
    // кластером). Прерванный init забывается.
    this.#initOpen = false;
  }

  /** Размер элемента: число, или `null` — «неизвестен» (все биты значения — единицы). */
  #readSize(idLength: number, sizeLength: number): number | null {
    // Значение — все биты записи без маркера длины.
    const markerBit = 0x80 >> (sizeLength - 1);
    let value = this.header.byteAt(idLength) & (markerBit - 1);
    let allOnes = value === markerBit - 1;
    for (let index = idLength + 1; index < idLength + sizeLength; index += 1) {
      const byte = this.header.byteAt(index);
      value = value * 256 + byte;
      allOnes = allOnes && byte === 0xff;
    }
    return allOnes ? null : value;
  }

  #dispatch(elementId: number, size: number | null, headerStart: number, markers: SegmentMarker[]): HeaderStep {
    if (size !== null && !Number.isSafeInteger(size)) return { kind: "failed", reason: "ebml element size is too large" };

    if (elementId === EBML_HEADER_ID) {
      // Новый поток (первый или смена качества): здесь начинается init-сегмент.
      if (size === null) return { kind: "failed", reason: "ebml header of unknown size" };
      if (!this.#initOpen) markers.push({ kind: "init_start", offset: headerStart });
      this.#initOpen = true;
      this.#level = "top";
      return { kind: "skip_body", bodyBytes: size, markerAtBodyEnd: null };
    }
    if (elementId === SEGMENT_ID) {
      // В Segment входим: его тело — элементы уровня 1, они разбираются дальше.
      this.#level = "segment";
      return { kind: "skip_body", bodyBytes: 0, markerAtBodyEnd: null };
    }
    if (this.#level === "top") return { kind: "failed", reason: "stream does not start like webm" };

    if (elementId === CLUSTER_ID) {
      if (this.#initOpen) {
        markers.push({ kind: "init_end", offset: headerStart });
        this.#initOpen = false;
      }
      markers.push({ kind: "media_start", offset: headerStart });
      // Кластер неизвестного размера: входим в него, дети пойдут как обычные элементы.
      return { kind: "skip_body", bodyBytes: size ?? 0, markerAtBodyEnd: null };
    }
    // Любой другой элемент уровня 1 или ребёнок кластера неизвестного размера.
    if (size === null) return { kind: "failed", reason: "element of unknown size outside Segment/Cluster" };
    return { kind: "skip_body", bodyBytes: size, markerAtBodyEnd: null };
  }
}
