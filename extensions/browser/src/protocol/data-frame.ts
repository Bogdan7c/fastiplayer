// Двоичная рамка куска байтов - двоичная рамка WebSocket сеанса. Зеркало data_frame.rs.
//
// Байты не кодируются в base64 и не заворачиваются в JSON. Заголовок фиксированный,
// 24 байта, все числа little-endian:
//
//   смещение  размер  поле
//    0        1       вид рамки: 1 - байты SourceBuffer, 2 - байты файла <video src>
//    1        1       флаги - пока 0
//    2        2       резерв - 0
//   вид 1, байты SourceBuffer:
//    4        4       поколение (от 1)
//    8        2       индекс SourceBuffer (0-7)
//   10        6       резерв - 0
//   16        8       порядковый номер рамки в SourceBuffer поколения (u64 -> bigint)
//   вид 2, байты файла <video src> (ответ на fetch_range):
//    4        4       номер запроса
//    8        8       смещение первого байта в файле (до 2^53 - 1)
//   16        8       резерв - 0
//   24        1...1 МиБ полезный груз
//
// Ненулевой резерв - ошибка: если будущая minor займёт резерв, её рамки старой
// стороне никто не пошлёт.

import {
  type DataFrameErrorKind,
  type DecodeResult,
  type EncodeResult,
  reject,
  succeed,
} from "./errors.ts";
import {
  DATA_FRAME_HEADER_BYTES,
  MAX_DATA_FRAME_PAYLOAD_BYTES,
  MAX_JSON_SAFE_INTEGER,
  MAX_SOURCE_BUFFERS_PER_GENERATION,
} from "./limits.ts";

const SOURCE_BUFFER_BYTES_KIND = 1;
const RANGE_BYTES_KIND = 2;
const MAX_U32 = 0xffff_ffff;
const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const LITTLE_ENDIAN = true;

/** Заголовок рамки с байтами SourceBuffer. */
export interface SourceBufferBytesHeader {
  readonly kind: "source_buffer_bytes";
  /** Поколение, 1..2^32-1. */
  readonly generation: number;
  /** Индекс SourceBuffer, 0..7. */
  readonly source_buffer: number;
  /** Порядковый номер рамки (с 0 для каждого SourceBuffer поколения), 64 бита. */
  readonly sequence: bigint;
}

/** Заголовок рамки с байтами файла `<video src>`. */
export interface RangeBytesHeader {
  readonly kind: "range_bytes";
  /** Номер запроса `fetch_range`, u32. */
  readonly request_id: number;
  /** Смещение первого байта в файле, 0..2^53-1. */
  readonly offset: number;
}

export type DataFrameHeader = SourceBufferBytesHeader | RangeBytesHeader;

/** Разобранная рамка. `payload` - подмассив входного буфера, без копии. */
export interface DataFrame {
  readonly header: DataFrameHeader;
  readonly payload: Uint8Array;
}

function checkPayloadSize(payloadLength: number): DataFrameErrorKind | undefined {
  if (payloadLength === 0) return "payload_empty";
  if (payloadLength > MAX_DATA_FRAME_PAYLOAD_BYTES) return "payload_too_large";
  return undefined;
}

/** Ошибка разбора заголовка или `undefined`, если разбор удался (значение кладётся в `parsed`). */
function decodeHeader(view: DataView): DecodeResult<DataFrameHeader> {
  const kind = view.getUint8(0);
  // Резерв проверяется раньше вида, как в Rust: флаги и два байта резерва всегда нули.
  if (view.getUint8(1) !== 0 || view.getUint8(2) !== 0 || view.getUint8(3) !== 0) {
    return reject("reserved_bits_set", "header bytes 1-3");
  }
  if (kind === SOURCE_BUFFER_BYTES_KIND) {
    return decodeSourceBufferHeader(view);
  }
  if (kind === RANGE_BYTES_KIND) {
    return decodeRangeHeader(view);
  }
  return reject("unknown_frame_kind", "header byte 0");
}

function decodeSourceBufferHeader(view: DataView): DecodeResult<DataFrameHeader> {
  if (view.getUint16(10, LITTLE_ENDIAN) !== 0 || view.getUint32(12, LITTLE_ENDIAN) !== 0) {
    return reject("reserved_bits_set", "header bytes 10-15");
  }
  const generation = view.getUint32(4, LITTLE_ENDIAN);
  if (generation === 0) {
    return reject("number_out_of_range", "generation");
  }
  const sourceBuffer = view.getUint16(8, LITTLE_ENDIAN);
  if (sourceBuffer >= MAX_SOURCE_BUFFERS_PER_GENERATION) {
    return reject("number_out_of_range", "source_buffer");
  }
  return succeed({
    kind: "source_buffer_bytes",
    generation,
    source_buffer: sourceBuffer,
    sequence: view.getBigUint64(16, LITTLE_ENDIAN),
  });
}

function decodeRangeHeader(view: DataView): DecodeResult<DataFrameHeader> {
  if (view.getBigUint64(16, LITTLE_ENDIAN) !== 0n) {
    return reject("reserved_bits_set", "header bytes 16-23");
  }
  const offset = view.getBigUint64(8, LITTLE_ENDIAN);
  if (offset > BigInt(MAX_JSON_SAFE_INTEGER)) {
    return reject("number_out_of_range", "offset");
  }
  return succeed({ kind: "range_bytes", request_id: view.getUint32(4, LITTLE_ENDIAN), offset: Number(offset) });
}

/**
 * Разбирает двоичную рамку. Порядок проверок как в Rust: короче заголовка ->
 * размер груза (пустой / больше 1 МиБ) -> резерв -> вид -> поля.
 */
export function decodeDataFrame(bytes: Uint8Array): DecodeResult<DataFrame> {
  if (bytes.length < DATA_FRAME_HEADER_BYTES) {
    return reject("frame_too_short", "frame");
  }
  const payload = bytes.subarray(DATA_FRAME_HEADER_BYTES);
  const payloadProblem = checkPayloadSize(payload.length);
  if (payloadProblem !== undefined) {
    return reject(payloadProblem, "payload");
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, DATA_FRAME_HEADER_BYTES);
  const header = decodeHeader(view);
  return header.ok ? succeed({ header: header.value, payload }) : header;
}

/** Записывает поля заголовка в 24 байта; значения вне диапазона - `number_out_of_range`. */
function writeHeader(header: DataFrameHeader, view: DataView): DecodeResult<true> {
  if (header.kind === "source_buffer_bytes") {
    const valid =
      Number.isInteger(header.generation) &&
      header.generation >= 1 &&
      header.generation <= MAX_U32 &&
      Number.isInteger(header.source_buffer) &&
      header.source_buffer >= 0 &&
      header.source_buffer < MAX_SOURCE_BUFFERS_PER_GENERATION &&
      typeof header.sequence === "bigint" &&
      header.sequence >= 0n &&
      header.sequence <= MAX_U64;
    if (!valid) return reject("number_out_of_range", "source_buffer_bytes header");
    view.setUint8(0, SOURCE_BUFFER_BYTES_KIND);
    view.setUint32(4, header.generation, LITTLE_ENDIAN);
    view.setUint16(8, header.source_buffer, LITTLE_ENDIAN);
    view.setBigUint64(16, header.sequence, LITTLE_ENDIAN);
    return succeed(true);
  }
  const valid =
    Number.isInteger(header.request_id) &&
    header.request_id >= 0 &&
    header.request_id <= MAX_U32 &&
    Number.isInteger(header.offset) &&
    header.offset >= 0 &&
    header.offset <= MAX_JSON_SAFE_INTEGER;
  if (!valid) return reject("number_out_of_range", "range_bytes header");
  view.setUint8(0, RANGE_BYTES_KIND);
  view.setUint32(4, header.request_id, LITTLE_ENDIAN);
  view.setBigUint64(8, BigInt(header.offset), LITTLE_ENDIAN);
  return succeed(true);
}

/** Собирает рамку: 24 байта заголовка и груз (1 байт...1 МиБ). Груз копируется один раз. */
export function encodeDataFrame(header: DataFrameHeader, payload: Uint8Array): EncodeResult<Uint8Array> {
  const payloadProblem = checkPayloadSize(payload.length);
  if (payloadProblem !== undefined) {
    return reject(payloadProblem, "payload");
  }
  const frame = new Uint8Array(DATA_FRAME_HEADER_BYTES + payload.length);
  const written = writeHeader(header, new DataView(frame.buffer, 0, DATA_FRAME_HEADER_BYTES));
  if (!written.ok) {
    return written;
  }
  frame.set(payload, DATA_FRAME_HEADER_BYTES);
  return succeed(frame);
}
