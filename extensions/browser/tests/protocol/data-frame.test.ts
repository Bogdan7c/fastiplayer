// Двоичные рамки: границы заголовка и груза сверх общих фикстур.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DATA_FRAME_HEADER_BYTES,
  MAX_DATA_FRAME_PAYLOAD_BYTES,
  decodeDataFrame,
  encodeDataFrame,
  type DataFrameHeader,
} from "../../src/protocol/index.ts";

const SOURCE_HEADER: DataFrameHeader = { kind: "source_buffer_bytes", generation: 3, source_buffer: 1, sequence: 41n };

function frameWith(header: Uint8Array | number[], payloadLength: number): Uint8Array {
  const frame = new Uint8Array(DATA_FRAME_HEADER_BYTES + payloadLength);
  frame.set(header);
  return frame;
}

test("рамка SourceBuffer: заголовок читается по смещениям little-endian", () => {
  const payload = new Uint8Array([1, 2, 3, 4, 5]);
  const encoded = encodeDataFrame(SOURCE_HEADER, payload);
  assert.ok(encoded.ok);
  const bytes = encoded.value;
  assert.equal(bytes.length, DATA_FRAME_HEADER_BYTES + 5);
  assert.deepEqual([...bytes.subarray(0, 4)], [1, 0, 0, 0]);
  assert.deepEqual([...bytes.subarray(4, 8)], [3, 0, 0, 0]);
  assert.deepEqual([...bytes.subarray(8, 10)], [1, 0]);
  assert.deepEqual([...bytes.subarray(10, 16)], [0, 0, 0, 0, 0, 0]);
  assert.deepEqual([...bytes.subarray(16, 24)], [41, 0, 0, 0, 0, 0, 0, 0]);
  assert.deepEqual([...bytes.subarray(24)], [1, 2, 3, 4, 5]);
});

test("рамка файла: заголовок читается по смещениям little-endian, смещение 2^53-1 точное", () => {
  const encoded = encodeDataFrame({ kind: "range_bytes", request_id: 0x01020304, offset: Number.MAX_SAFE_INTEGER }, new Uint8Array([9]));
  assert.ok(encoded.ok);
  assert.deepEqual([...encoded.value.subarray(0, 8)], [2, 0, 0, 0, 4, 3, 2, 1]);
  assert.deepEqual([...encoded.value.subarray(8, 16)], [255, 255, 255, 255, 255, 255, 31, 0]);
  assert.deepEqual([...encoded.value.subarray(16, 24)], [0, 0, 0, 0, 0, 0, 0, 0]);
});

test("разбор не копирует груз: payload - подмассив входного буфера", () => {
  const encoded = encodeDataFrame(SOURCE_HEADER, new Uint8Array([7, 8, 9]));
  assert.ok(encoded.ok);
  const decoded = decodeDataFrame(encoded.value);
  assert.ok(decoded.ok);
  assert.equal(decoded.value.payload.buffer, encoded.value.buffer);
  assert.equal(decoded.value.payload.byteOffset, DATA_FRAME_HEADER_BYTES);
  assert.deepEqual([...decoded.value.payload], [7, 8, 9]);
});

test("разбор работает на подмассиве с ненулевым смещением в общем буфере", () => {
  const encoded = encodeDataFrame(SOURCE_HEADER, new Uint8Array([7, 8, 9]));
  assert.ok(encoded.ok);
  const shared = new Uint8Array(100);
  shared.set(encoded.value, 13);
  const decoded = decodeDataFrame(shared.subarray(13, 13 + encoded.value.length));
  assert.ok(decoded.ok);
  assert.deepEqual(decoded.value.header, SOURCE_HEADER);
  assert.deepEqual([...decoded.value.payload], [7, 8, 9]);
});

test("64-битный номер рамки не теряет точности выше 2^53", () => {
  const header: DataFrameHeader = { kind: "source_buffer_bytes", generation: 1, source_buffer: 0, sequence: 9_007_199_254_740_993n };
  const encoded = encodeDataFrame(header, new Uint8Array([1]));
  assert.ok(encoded.ok);
  const decoded = decodeDataFrame(encoded.value);
  assert.ok(decoded.ok);
  assert.deepEqual(decoded.value.header, header);
});

test("груз: ровно 1 МиБ принимается, на байт больше и пустой - нет (разбор и запись)", () => {
  const atLimit = frameWith([1, 0, 0, 0, 1, 0, 0, 0], MAX_DATA_FRAME_PAYLOAD_BYTES);
  assert.equal(decodeDataFrame(atLimit).ok, true);
  const over = frameWith([1, 0, 0, 0, 1, 0, 0, 0], MAX_DATA_FRAME_PAYLOAD_BYTES + 1);
  const overResult = decodeDataFrame(over);
  assert.equal(overResult.ok === false && overResult.error.kind, "payload_too_large");
  const emptyEncode = encodeDataFrame(SOURCE_HEADER, new Uint8Array(0));
  assert.equal(emptyEncode.ok === false && emptyEncode.error.kind, "payload_empty");
  const bigEncode = encodeDataFrame(SOURCE_HEADER, new Uint8Array(MAX_DATA_FRAME_PAYLOAD_BYTES + 1));
  assert.equal(bigEncode.ok === false && bigEncode.error.kind, "payload_too_large");
  assert.equal(encodeDataFrame(SOURCE_HEADER, new Uint8Array(MAX_DATA_FRAME_PAYLOAD_BYTES)).ok, true);
});

test("порядок проверок как в Rust: короче заголовка, груз, резерв, вид", () => {
  const kindKindOf = (bytes: Uint8Array): string => {
    const result = decodeDataFrame(bytes);
    return result.ok ? "ok" : result.error.kind;
  };
  assert.equal(kindKindOf(new Uint8Array(23)), "frame_too_short");
  // Заголовок без груза: груз проверяется раньше резерва и вида.
  assert.equal(kindKindOf(new Uint8Array(24).fill(0xff)), "payload_empty");
  // Резерв (флаги) проверяется раньше вида: вид 9 с флагами - reserved_bits_set.
  assert.equal(kindKindOf(frameWith([9, 1], 1)), "reserved_bits_set");
  assert.equal(kindKindOf(frameWith([9], 1)), "unknown_frame_kind");
  assert.equal(kindKindOf(frameWith([1, 0, 0, 1, 1, 0, 0, 0], 1)), "reserved_bits_set");
});

test("запись: значения заголовка вне диапазона - number_out_of_range", () => {
  const payload = new Uint8Array([1]);
  const bad: DataFrameHeader[] = [
    { kind: "source_buffer_bytes", generation: 0, source_buffer: 0, sequence: 0n },
    { kind: "source_buffer_bytes", generation: 2 ** 32, source_buffer: 0, sequence: 0n },
    { kind: "source_buffer_bytes", generation: 1, source_buffer: 8, sequence: 0n },
    { kind: "source_buffer_bytes", generation: 1, source_buffer: 0, sequence: -1n },
    { kind: "source_buffer_bytes", generation: 1, source_buffer: 0, sequence: 2n ** 64n },
    { kind: "range_bytes", request_id: -1, offset: 0 },
    { kind: "range_bytes", request_id: 2 ** 32, offset: 0 },
    { kind: "range_bytes", request_id: 1, offset: 2 ** 53 },
    { kind: "range_bytes", request_id: 1, offset: 1.5 },
  ];
  for (const header of bad) {
    const result = encodeDataFrame(header, payload);
    assert.equal(result.ok === false && result.error.kind, "number_out_of_range", JSON.stringify(header, (_, v: unknown) => (typeof v === "bigint" ? `${v}n` : v)));
  }
  // Максимумы допустимы.
  assert.equal(encodeDataFrame({ kind: "source_buffer_bytes", generation: 2 ** 32 - 1, source_buffer: 7, sequence: 2n ** 64n - 1n }, payload).ok, true);
});
