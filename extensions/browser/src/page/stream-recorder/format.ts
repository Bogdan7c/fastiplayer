// Формат записи страницы `*.fprec` версии 1 (контракт с читателем в стенде, пакет П3 сессии 09b2).
//
// Файл: 8 байт магии `FPREC001`, u32 little-endian — длина манифеста N, N байт UTF-8 JSON-манифеста,
// затем «склад байтов»: подряд полезные нагрузки всех `append`. Событие `append` ссылается на склад
// парой `offset`/`length`. Здесь — только описание формата и его кодирование; сбор событий — в
// `recorder.ts`.
//
// ВАЖНО: это ОТЛАДОЧНЫЙ инструмент. В продуктовой сборке этого кода нет, а запись с байтами сайта
// существует только в памяти страницы, пока владелец сам не выгрузит её файлом.

/** Магия начала файла. */
export const FPREC_MAGIC = "FPREC001";

/** Значение поля `format` манифеста. */
export const FPREC_FORMAT_NAME = "fastiplayer-page-stream-recording";

/** Версия формата. */
export const FPREC_VERSION = 1;

/** Потолок склада байтов по умолчанию: 512 МиБ. Дальше — `truncated: true`, события без байтов пишутся. */
export const DEFAULT_STORE_CEILING_BYTES = 512 * 1024 * 1024;

/** Потолок числа событий (страховка памяти страницы: события без байтов тоже занимают память). */
export const MAX_RECORDED_EVENTS = 2_000_000;

/** Значение JSON, допустимое в событии. */
export type JsonValue = string | number | boolean | null | readonly JsonValue[] | { readonly [key: string]: JsonValue };

/** Одно событие записи: обязательные `t_ms`, `ctx`, `op` и поля операции. */
export interface RecordedEvent {
  t_ms: number;
  ctx: string;
  op: string;
  [field: string]: JsonValue;
}

/** Браузер, в котором сделана запись. */
export type RecordedBrowser = "firefox" | "chrome";

/** Манифест файла. */
export interface RecordingManifest {
  readonly format: typeof FPREC_FORMAT_NAME;
  readonly version: typeof FPREC_VERSION;
  readonly browser: RecordedBrowser;
  /** Только origin страницы: без пути и запроса. */
  readonly origin: string;
  readonly started_at_unix_ms: number;
  readonly truncated: boolean;
  readonly events: readonly RecordedEvent[];
}

/**
 * Собрать файл из манифеста и кусков склада. Возвращает части для `Blob` (склад не склеивается в
 * один буфер: он может занимать сотни мегабайт).
 */
export function encodeRecordingParts(manifest: RecordingManifest, storeChunks: readonly Uint8Array[]): Uint8Array[] {
  const textEncoder = new TextEncoder();
  const manifestBytes = textEncoder.encode(JSON.stringify(manifest));
  const header = new Uint8Array(FPREC_MAGIC.length + 4);
  header.set(textEncoder.encode(FPREC_MAGIC), 0);
  // u32 little-endian: длина манифеста.
  new DataView(header.buffer).setUint32(FPREC_MAGIC.length, manifestBytes.length, true);
  return [header, manifestBytes, ...storeChunks];
}

/** FNV-1a 32 бит над байтами — 8 hex-цифр строчными (договор формата для `append` и `sent`). */
export function fnv1a32Hex(bytes: Uint8Array): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < bytes.length; index += 1) {
    hash ^= bytes[index] as number;
    // Умножение на простое 0x01000193 по модулю 2^32 без потери точности.
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

/** Число для JSON: конечное — числом, бесконечность и NaN — строкой (`"Infinity"`, `"NaN"`), как требует формат. */
export function jsonNumber(value: number): number | string {
  return Number.isFinite(value) ? value : String(value);
}
