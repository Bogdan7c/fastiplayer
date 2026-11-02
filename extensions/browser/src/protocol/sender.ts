// Помощники отправителя: что экстеншен делает с данными ДО отправки (поправки по итогам 01).
//
// Протокол строгий и «негодный» заголовок или слишком крупную рамку не исправляет,
// а отвергает. Поэтому чистит и режет сам экстеншен.

import { MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_PAGE_TITLE_BYTES } from "./limits.ts";
import { isAllowedDisplayCodePoint } from "./text.ts";

/**
 * Символы-разрывы строки и табуляции: в заголовке они разделяют слова, поэтому
 * заменяются одним пробелом, а не выбрасываются (иначе «Серія 3\nОзвучка» склеится
 * в «Серія 3Озвучка»). Сюда входят TAB, LF, VT, FF, CR, NEL (U+0085) и
 * разделители строки/абзаца U+2028/2029.
 */
function isWordSeparatingBreak(codePoint: number): boolean {
  return (
    (codePoint >= 0x09 && codePoint <= 0x0d) || codePoint === 0x85 || codePoint === 0x2028 || codePoint === 0x2029
  );
}

/**
 * Готовит заголовок страницы к отправке:
 * - разрывы строки и табуляцию заменяет одним пробелом (подряд идущие разрывы и пробелы
 *   перед ними дают ровно один пробел);
 * - прочие управляющие символы, символы переворота направления текста
 *   (U+202A-202E, U+2066-2069) и одиночные суррогаты убирает;
 * - обрезает до 1024 байт UTF-8 по границе символа.
 * Результат всегда проходит строгую проверку текста `title` в `open_session`.
 */
export function sanitizePageTitle(rawTitle: string): string {
  let cleaned = "";
  let byteLength = 0;
  // Итерация по строке идёт кодовыми точками; одиночный суррогат приходит как отдельный символ.
  for (const rawCharacter of rawTitle) {
    const rawCodePoint = rawCharacter.codePointAt(0) ?? 0;
    if (isWordSeparatingBreak(rawCodePoint) && cleaned.endsWith(" ")) {
      continue; // пробел-разделитель уже стоит: второй не нужен
    }
    // Разрыв строки становится пробелом, дальше проверяется как обычный символ.
    const character = isWordSeparatingBreak(rawCodePoint) ? " " : rawCharacter;
    const codePoint = character.codePointAt(0) ?? 0;
    const isLoneSurrogate = codePoint >= 0xd800 && codePoint <= 0xdfff;
    if (isLoneSurrogate || !isAllowedDisplayCodePoint(codePoint)) {
      continue;
    }
    const characterBytes = codePoint < 0x80 ? 1 : codePoint < 0x800 ? 2 : codePoint < 0x10000 ? 3 : 4;
    if (byteLength + characterBytes > MAX_PAGE_TITLE_BYTES) {
      break; // следующий символ не помещается целиком: обрезаем по границе символа
    }
    cleaned += character;
    byteLength += characterBytes;
  }
  return cleaned;
}

/**
 * Режет крупный `appendBuffer` на куски не больше `maxChunkBytes` (по умолчанию 1 МиБ -
 * предел груза рамки). Куски - подмассивы того же буфера, без копий. Пустой вход даёт
 * пустой список: рамка с пустым грузом недопустима.
 */
export function splitAppendPayload(
  payload: Uint8Array | ArrayBuffer | ArrayBufferView,
  maxChunkBytes: number = MAX_DATA_FRAME_PAYLOAD_BYTES,
): Uint8Array[] {
  if (!Number.isInteger(maxChunkBytes) || maxChunkBytes < 1 || maxChunkBytes > MAX_DATA_FRAME_PAYLOAD_BYTES) {
    throw new RangeError(`maxChunkBytes must be an integer in 1..${MAX_DATA_FRAME_PAYLOAD_BYTES}`);
  }
  const bytes = asBytes(payload);
  const chunks: Uint8Array[] = [];
  for (let start = 0; start < bytes.length; start += maxChunkBytes) {
    chunks.push(bytes.subarray(start, Math.min(start + maxChunkBytes, bytes.length)));
  }
  return chunks;
}

/** Представление любого буфера байтов как `Uint8Array` без копирования. */
function asBytes(payload: Uint8Array | ArrayBuffer | ArrayBufferView): Uint8Array {
  if (payload instanceof Uint8Array) return payload;
  if (ArrayBuffer.isView(payload)) return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
  return new Uint8Array(payload);
}
