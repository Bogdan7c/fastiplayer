// Записи переменной длины EBML (RFC 8794, разделы 4 и 5): общий читатель для сканера границ WebM
// (webm-scanner.ts) и чтения времени сегментов (webm-segment-time.ts).
//
// ID элемента — 1–4 байта, размер — 1–8 байт. Длина записи задаётся числом ведущих нулевых бит
// первого байта (первый единичный бит — «маркер длины»). У размера все биты значения, равные единице,
// означают «размер неизвестен».

/** Самая длинная запись ID в EBML — 4 байта. */
export const MAX_EBML_ID_BYTES = 4;
/** Самая длинная запись размера в EBML — 8 байт. */
export const MAX_EBML_SIZE_BYTES = 8;

/** Длина записи переменной длины по первому байту (число ведущих нулей + 1); 0 — запись неверна. */
export function variableLengthOf(firstByte: number, maxBytes: number): number {
  for (let length = 1; length <= maxBytes; length += 1) {
    if (firstByte & (0x80 >> (length - 1))) return length;
  }
  return 0;
}

/**
 * Значение размера элемента из `length` байт, начиная с позиции `start` (читатель байт — `byteAt`).
 * `null` — «размер неизвестен» (все биты значения — единицы). Значение может не поместиться в безопасное
 * целое (8 байт): проверка на это — дело вызывающего (`Number.isSafeInteger`).
 */
export function ebmlSizeValue(byteAt: (index: number) => number, start: number, length: number): number | null {
  // Значение — все биты записи без маркера длины.
  const markerBit = 0x80 >> (length - 1);
  let value = byteAt(start) & (markerBit - 1);
  let allOnes = value === markerBit - 1;
  for (let index = start + 1; index < start + length; index += 1) {
    const byte = byteAt(index);
    value = value * 256 + byte;
    allOnes = allOnes && byte === 0xff;
  }
  return allOnes ? null : value;
}
