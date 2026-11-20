// Время кластеров WebM и шкала TimecodeScale (решение 379) на вручную собранных байтах EBML: обычные и
// неизвестные размеры, Timestamp не первым, нет TimecodeScale, битые и обрезанные байты. Ожидаемые числа
// посчитаны по спецификации Matroska/EBML, а не кодом экстеншена.
import assert from "node:assert/strict";
import { test } from "node:test";

import type { StreamByteReader } from "../../src/page/segment-scanner/fmp4-segment-time.ts";
import { readWebmClusterTimestamp, readWebmTimecodeScale, webmSecondsOf } from "../../src/page/segment-scanner/webm-segment-time.ts";
import { concat, EBML_IDS, ebmlElement, ebmlUnknownSizeHeader, filler, MUXED_WEBM } from "./support/media-samples.ts";

const TIMECODE_SCALE_ID = 0x2ad7b1;
const VOID_ID = 0xec;
const CRC32_ID = 0xbf;

/** Читатель байт поверх массива: `null`, если запрошено за концом (как у владельца журнала). */
function readerOf(bytes: Uint8Array): StreamByteReader {
  return (offset, length) => (offset >= 0 && offset + length <= bytes.length ? bytes.slice(offset, offset + length) : null);
}

/** Беззнаковое число в `count` байт, старший первым. */
function unsigned(value: number, count: number): Uint8Array {
  const bytes = new Uint8Array(count);
  let rest = value;
  for (let index = count - 1; index >= 0; index -= 1) {
    bytes[index] = rest % 256;
    rest = Math.floor(rest / 256);
  }
  return bytes;
}

function timestampElement(value: number, count: number): Uint8Array {
  return ebmlElement(EBML_IDS.timestamp, unsigned(value, count));
}

function clusterOf(children: readonly Uint8Array[]): Uint8Array {
  return ebmlElement(EBML_IDS.cluster, concat(children));
}

/** Init: EBML-заголовок, Segment (известного или неизвестного размера), Info с заданными детьми, Tracks. */
function initOf(options: { readonly infoChildren: readonly Uint8Array[]; readonly unknownSizeSegment?: boolean; readonly withInfo?: boolean }): Uint8Array {
  const inside = [
    ...(options.withInfo === false ? [] : [ebmlElement(EBML_IDS.info, concat(options.infoChildren))]),
    ebmlElement(EBML_IDS.tracks, filler(30)),
  ];
  const segment = options.unknownSizeSegment === true ? concat([ebmlUnknownSizeHeader(EBML_IDS.segment), ...inside]) : ebmlElement(EBML_IDS.segment, concat(inside));
  return concat([ebmlElement(EBML_IDS.header, filler(19)), segment]);
}

function scaleOf(init: Uint8Array): number | null {
  return readWebmTimecodeScale(readerOf(init), 0, init.length);
}

test("cluster timestamp: first child, 1 to 8 bytes", () => {
  for (const count of [1, 2, 3, 4, 6, 8]) {
    const cluster = clusterOf([timestampElement(200, count), ebmlElement(EBML_IDS.simpleBlock, filler(40))]);
    assert.equal(readWebmClusterTimestamp(readerOf(cluster), 0), 200, `${count}-byte Timestamp`);
  }
  assert.equal(readWebmClusterTimestamp(readerOf(clusterOf([timestampElement(0x1_0000_0000, 5)])), 0), 0x1_0000_0000);
});

test("cluster timestamp: read at a nonzero stream offset", () => {
  const stream = concat([filler(100), clusterOf([timestampElement(7_000, 2)])]);
  assert.equal(readWebmClusterTimestamp(readerOf(stream), 100), 7_000);
});

test("cluster timestamp: not the first child (CRC-32, Void before it)", () => {
  const cluster = clusterOf([ebmlElement(CRC32_ID, filler(4)), ebmlElement(VOID_ID, filler(9)), timestampElement(1234, 2)]);
  assert.equal(readWebmClusterTimestamp(readerOf(cluster), 0), 1234);
});

test("cluster timestamp: unknown-size cluster (live) is read the same way", () => {
  const live = concat([ebmlUnknownSizeHeader(EBML_IDS.cluster), timestampElement(5_000, 2), ebmlElement(EBML_IDS.simpleBlock, filler(30))]);
  assert.equal(readWebmClusterTimestamp(readerOf(live), 0), 5_000);
});

test("cluster timestamp: null when it is not a cluster, has no Timestamp, or Timestamp is malformed", () => {
  // Это не Cluster, а Cues.
  assert.equal(readWebmClusterTimestamp(readerOf(ebmlElement(EBML_IDS.cues, filler(10))), 0), null);
  // В кластере нет Timestamp.
  assert.equal(readWebmClusterTimestamp(readerOf(clusterOf([ebmlElement(EBML_IDS.simpleBlock, filler(30))])), 0), null);
  // Timestamp нулевой длины и длиной 9 байт — не число EBML.
  assert.equal(readWebmClusterTimestamp(readerOf(clusterOf([ebmlElement(EBML_IDS.timestamp, new Uint8Array(0))])), 0), null);
  assert.equal(readWebmClusterTimestamp(readerOf(clusterOf([ebmlElement(EBML_IDS.timestamp, new Uint8Array(9))])), 0), null);
  // 8 байт, не помещающихся в безопасное целое.
  assert.equal(readWebmClusterTimestamp(readerOf(clusterOf([ebmlElement(EBML_IDS.timestamp, new Uint8Array(8).fill(0xff))])), 0), null);
  // Мусор: ID с нулевым первым байтом.
  assert.equal(readWebmClusterTimestamp(readerOf(filler(40, 0).fill(0, 0, 8)), 0), null);
});

test("cluster timestamp: an unknown-size child before Timestamp cannot be skipped, so null", () => {
  const cluster = concat([ebmlUnknownSizeHeader(EBML_IDS.cluster), ebmlUnknownSizeHeader(VOID_ID), timestampElement(5, 1)]);
  assert.equal(readWebmClusterTimestamp(readerOf(cluster), 0), null);
});

test("cluster timestamp: a sibling-count limit stops a long run of Void elements", () => {
  const voids = Array.from({ length: 100 }, () => ebmlElement(VOID_ID, filler(1)));
  assert.equal(readWebmClusterTimestamp(readerOf(clusterOf([...voids, timestampElement(9, 1)])), 0), null);
});

test("cluster timestamp: truncated bytes give null at every cut", () => {
  const cluster = clusterOf([timestampElement(300, 2), ebmlElement(EBML_IDS.simpleBlock, filler(20))]);
  // Заголовок кластера (4 + 4) и Timestamp (1 + 4 + 2) занимают первые 15 байт: короче — времени нет.
  for (let cut = 0; cut < 15; cut += 1) {
    assert.equal(readWebmClusterTimestamp(readerOf(cluster.slice(0, cut)), 0), null, `cut at ${cut}`);
  }
  assert.equal(readWebmClusterTimestamp(readerOf(cluster.slice(0, 15)), 0), 300);
});

test("timecode scale: read from Info, known and unknown-size Segment", () => {
  const info = [ebmlElement(0x4d80, filler(5)), ebmlElement(TIMECODE_SCALE_ID, unsigned(1_000_000, 3))];
  assert.equal(scaleOf(initOf({ infoChildren: info })), 1_000_000);
  assert.equal(scaleOf(initOf({ infoChildren: [ebmlElement(TIMECODE_SCALE_ID, unsigned(500_000, 3))], unknownSizeSegment: true })), 500_000);
  assert.equal(scaleOf(initOf({ infoChildren: [ebmlElement(TIMECODE_SCALE_ID, unsigned(1_000, 2))] })), 1_000);
});

test("timecode scale: Info without TimecodeScale means the 1 ms default; no Info or a zero scale is unknown", () => {
  assert.equal(scaleOf(initOf({ infoChildren: [ebmlElement(0x4d80, filler(5))] })), 1_000_000);
  assert.equal(scaleOf(initOf({ infoChildren: [], withInfo: false })), null);
  assert.equal(scaleOf(initOf({ infoChildren: [ebmlElement(TIMECODE_SCALE_ID, unsigned(0, 3))] })), null);
});

test("timecode scale: truncated or garbage init gives null", () => {
  const init = initOf({ infoChildren: [ebmlElement(TIMECODE_SCALE_ID, unsigned(1_000_000, 3))] });
  // Режем до конца TimecodeScale (EBML 23 + Segment 8 + Info 8 + TimecodeScale 7): короче — шкалы не прочесть.
  for (let cut = 0; cut < 46; cut += 1) {
    assert.equal(readWebmTimecodeScale(readerOf(init.slice(0, cut)), 0, cut), null, `cut at ${cut}`);
  }
  assert.equal(readWebmTimecodeScale(readerOf(filler(60, 0).fill(0, 0, 8)), 0, 60), null);
});

test("seconds: ticks times scale over 1e9", () => {
  assert.equal(webmSecondsOf(2_400, 1_000_000), 2.4);
  assert.equal(webmSecondsOf(48_000, 1_000), 0.048);
});

test("a real ffmpeg WebM: init scale 1 ms and first cluster at 0 s", () => {
  // Сверено с ffprobe -show_packets: первый пакет pts ≈ 0 с (-0.007 из-за предзвона Opus), шкала muxed.webm — 1 мс.
  const webm = MUXED_WEBM.bytes();
  const read = readerOf(webm);
  assert.equal(readWebmTimecodeScale(read, 0, MUXED_WEBM.initEnd), 1_000_000);
  assert.equal(readWebmClusterTimestamp(read, MUXED_WEBM.initEnd), 0);
});
