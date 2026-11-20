// Время сегмента WebM/Matroska: с какой секунды потока начинается кластер (решение 379; для fMP4 то же
// делает fmp4-segment-time.ts).
//
// Зачем: журналу нужно знать, с какой секунды страницы начнётся повтор после выкидывания головы, иначе
// возврат к WebM-ролику с потерянным началом остаётся «неизвестным» (дыра решения 378).
//
// Как читаем (Matroska, https://www.matroska.org/technical/elements.html; EBML — RFC 8794):
// - Время кластера — его первый дочерний элемент `Timestamp` (ID 0xE7), беззнаковое число 1–8 байт в
//   «тиках сегмента». Спецификация: Timestamp ДОЛЖЕН быть первым ребёнком или вторым после `CRC-32`
//   (`SHOULD`), поэтому перед ним допускаем несколько элементов, перепрыгивая их по размерам.
// - Тик сегмента — `TimecodeScale` нс (ID 0x2AD7B1 внутри `Info`, ID 0x1549A966 внутри `Segment`).
//   Нет элемента — по умолчанию 1 000 000 нс (миллисекунда).
// - Секунды = Timestamp × TimecodeScale / 1e9.
//
// Читаем лениво и только заголовки: байты достаёт `StreamByteReader` владельца (журнал, дорожка).
// Любая странность (ID не тот, размер неизвестен там, где его нет права быть, байтов нет, число соседей
// выше предела) — честное «время неизвестно» (`null`), как у fMP4.

import { ebmlSizeValue, MAX_EBML_ID_BYTES, MAX_EBML_SIZE_BYTES, variableLengthOf } from "./ebml-vint.ts";
import type { StreamByteReader } from "./fmp4-segment-time.ts";

const SEGMENT_ID = 0x18538067;
const INFO_ID = 0x1549a966;
const TIMECODE_SCALE_ID = 0x2ad7b1;
const CLUSTER_ID = 0x1f43b675;
const TIMESTAMP_ID = 0xe7;

/** Шкала по умолчанию (спецификация Matroska): 1 000 000 нс на тик, то есть миллисекунда. */
const DEFAULT_TIMECODE_SCALE_NS = 1_000_000;
const NANOSECONDS_PER_SECOND = 1e9;
/** Беззнаковое число EBML — от 1 до 8 байт. */
const MAX_UNSIGNED_BYTES = 8;
/**
 * Сколько элементов одного уровня просматриваем, прежде чем сдаться. Перед `Timestamp` в кластере бывают
 * `CRC-32`/`Void`; в `Segment` до `Info` — `SeekHead`/`Void`; в `Info` — десяток строковых полей. 64 — с
 * запасом, а мусорный поток не заставит ходить по нему бесконечно.
 */
const MAX_SIBLING_ELEMENTS = 64;

interface EbmlElement {
  readonly id: number;
  /** Смещение первого байта тела. */
  readonly bodyStart: number;
  /** Размер тела; `null` — неизвестен (бывает у `Segment` и `Cluster` живых потоков). */
  readonly bodySize: number | null;
}

/** Заголовок элемента на смещении `offset`; `null` — байтов нет или это не заголовок. */
function readElement(read: StreamByteReader, offset: number): EbmlElement | null {
  const first = read(offset, 1);
  if (first === null) return null;
  const idLength = variableLengthOf(first[0] ?? 0, MAX_EBML_ID_BYTES);
  if (idLength === 0) return null;
  const idBytes = read(offset, idLength);
  const sizeFirst = read(offset + idLength, 1);
  if (idBytes === null || sizeFirst === null) return null;
  const sizeLength = variableLengthOf(sizeFirst[0] ?? 0, MAX_EBML_SIZE_BYTES);
  if (sizeLength === 0) return null;
  const sizeBytes = read(offset + idLength, sizeLength);
  if (sizeBytes === null) return null;
  let id = 0;
  for (const byte of idBytes) id = id * 256 + byte;
  const bodySize = ebmlSizeValue((index) => sizeBytes[index - idLength] ?? 0, idLength, sizeLength);
  if (bodySize !== null && !Number.isSafeInteger(bodySize)) return null;
  return { id, bodyStart: offset + idLength + sizeLength, bodySize };
}

/** Смещение сразу за телом; `null` — размер неизвестен, перепрыгнуть нельзя. */
function endOf(element: EbmlElement): number | null {
  return element.bodySize === null ? null : element.bodyStart + element.bodySize;
}

/**
 * Дети с `start` до `end` (`null` — конец неизвестен). Обход кончается на первом непонятном заголовке,
 * на элементе неизвестного размера (перепрыгнуть его нельзя, дальше смещениям верить нельзя) и на
 * элементе, вылезающем за `end`.
 */
function* elementsBetween(read: StreamByteReader, start: number, end: number | null): Generator<EbmlElement> {
  let offset = start;
  for (let seen = 0; seen < MAX_SIBLING_ELEMENTS; seen += 1) {
    if (end !== null && offset >= end) return;
    const element = readElement(read, offset);
    if (element === null) return;
    const elementEnd = endOf(element);
    if (end !== null && elementEnd !== null && elementEnd > end) return;
    yield element;
    // Элемент неизвестного размера отдан, но следующего соседа после него определить нельзя.
    if (elementEnd === null) return;
    offset = elementEnd;
  }
}

/** Беззнаковое число тела элемента (1–8 байт); `null` — нет байтов, длина не та или число не безопасно. */
function readUnsignedBody(read: StreamByteReader, element: EbmlElement): number | null {
  const size = element.bodySize;
  if (size === null || size < 1 || size > MAX_UNSIGNED_BYTES) return null;
  const bytes = read(element.bodyStart, size);
  if (bytes === null) return null;
  let value = 0;
  for (const byte of bytes) value = value * 256 + byte;
  return Number.isSafeInteger(value) ? value : null;
}

/**
 * `Timestamp` кластера, начинающегося на смещении `segmentStart` (граница журнала: заголовок `Cluster`),
 * в тиках сегмента. `null` — здесь не кластер, `Timestamp` не найден среди первых детей или не разобран.
 */
export function readWebmClusterTimestamp(read: StreamByteReader, segmentStart: number): number | null {
  const cluster = readElement(read, segmentStart);
  if (cluster === null || cluster.id !== CLUSTER_ID) return null;
  for (const child of elementsBetween(read, cluster.bodyStart, endOf(cluster))) {
    if (child.id === TIMESTAMP_ID) return readUnsignedBody(read, child);
  }
  return null;
}

/**
 * `TimecodeScale` (нс на тик) init-сегмента, лежащего в потоке с `initStart` до `initEnd`: EBML-заголовок →
 * `Segment` → `Info` → `TimecodeScale`. `Info` найден, а `TimecodeScale` в нём нет — шкала по умолчанию.
 * `null` — `Segment` или `Info` не найдены, байты не разобраны или шкала нулевая.
 */
export function readWebmTimecodeScale(read: StreamByteReader, initStart: number, initEnd: number): number | null {
  // Верхний уровень без границы: тело Segment уходит далеко за init (в нём же лежат кластеры), и обход
  // с границей initEnd отверг бы сам Segment. Границей служит initEnd для его детей ниже.
  for (const top of elementsBetween(read, initStart, null)) {
    if (top.id !== SEGMENT_ID) continue;
    for (const child of elementsBetween(read, top.bodyStart, initEnd)) {
      if (child.id !== INFO_ID) continue;
      return timecodeScaleOfInfo(read, child);
    }
    return null;
  }
  return null;
}

function timecodeScaleOfInfo(read: StreamByteReader, info: EbmlElement): number | null {
  for (const child of elementsBetween(read, info.bodyStart, endOf(info))) {
    if (child.id !== TIMECODE_SCALE_ID) continue;
    const scale = readUnsignedBody(read, child);
    return scale === null || scale === 0 ? null : scale;
  }
  return DEFAULT_TIMECODE_SCALE_NS;
}

/** Секунды потока из `Timestamp` кластера (в тиках) и `TimecodeScale` (нс на тик). */
export function webmSecondsOf(clusterTimestamp: number, timecodeScaleNs: number): number {
  return (clusterTimestamp * timecodeScaleNs) / NANOSECONDS_PER_SECOND;
}
