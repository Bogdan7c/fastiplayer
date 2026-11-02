// Сканер границ сегментов: те же границы, как бы поток ни был порезан на куски.
//
// Ожидаемые смещения файлов фикстур взяты из независимого разбора (см. media-samples.ts), а живой
// WebM собран мини-писателем EBML, который знает смещения по построению.

import assert from "node:assert/strict";
import { test } from "node:test";

import { createSegmentScanner, type SegmentMarker } from "../../src/page/segment-scanner/scanner.ts";
import {
  AUDIO_MP4,
  MUXED_WEBM,
  VIDEO_64_6S_MP4,
  VIDEO_MP4,
  concat,
  ebmlElement,
  EBML_IDS,
  filler,
  liveWebm,
  randomChunks,
  splitAt,
} from "./support/media-samples.ts";

/** Прогнать поток по кускам; `null` — сканер сообщил сбой. */
function scanAll(mimeType: string, chunks: readonly Uint8Array[]): SegmentMarker[] | null {
  const scanner = createSegmentScanner(mimeType, 0);
  assert.ok(scanner !== null, `scanner for ${mimeType}`);
  const markers: SegmentMarker[] = [];
  for (const chunk of chunks) {
    const outcome = scanner.scan(chunk);
    if (outcome.kind === "failed") return null;
    markers.push(...outcome.markers);
  }
  return markers;
}

/** Ожидаемые метки потока: init с 0, конец init, начала медиасегментов. */
function expectedMarkers(sample: { readonly initEnd: number; readonly mediaStarts: readonly number[] }): SegmentMarker[] {
  const markers: SegmentMarker[] = [{ kind: "init_start", offset: 0 }, { kind: "init_end", offset: sample.initEnd }];
  for (const start of sample.mediaStarts) markers.push({ kind: "media_start", offset: start });
  return markers;
}

const SAMPLES = [
  { name: "fMP4 H.264 video.mp4", mime: 'video/mp4; codecs="avc1.42E01E"', sample: VIDEO_MP4, bytes: VIDEO_MP4.bytes() },
  { name: "fMP4 AAC audio.mp4", mime: 'audio/mp4; codecs="mp4a.40.2"', sample: AUDIO_MP4, bytes: AUDIO_MP4.bytes() },
  { name: "fMP4 6 s video-64-6s.mp4", mime: "video/mp4", sample: VIDEO_64_6S_MP4, bytes: VIDEO_64_6S_MP4.bytes() },
  { name: "WebM known sizes muxed.webm", mime: 'video/webm; codecs="vp9, opus"', sample: MUXED_WEBM, bytes: MUXED_WEBM.bytes() },
  { name: "WebM unknown sizes (live)", mime: "video/webm", sample: liveWebm(6), bytes: liveWebm(6).bytes },
];

for (const { name, mime, sample, bytes } of SAMPLES) {
  test(`${name}: whole stream gives the independently known boundaries`, () => {
    assert.deepEqual(scanAll(mime, [bytes]), expectedMarkers(sample));
  });

  test(`${name}: every two-piece cut gives the same boundaries`, () => {
    const expected = expectedMarkers(sample);
    for (let cut = 1; cut < bytes.length; cut += 1) {
      assert.deepEqual(scanAll(mime, splitAt(bytes, [cut])), expected, `cut at ${cut}`);
    }
  });

  test(`${name}: one-byte chunks and random chunks give the same boundaries`, () => {
    const expected = expectedMarkers(sample);
    assert.deepEqual(scanAll(mime, splitAt(bytes, Array.from({ length: bytes.length }, (_, index) => index))), expected);
    for (let seed = 1; seed <= 20; seed += 1) {
      assert.deepEqual(scanAll(mime, randomChunks(bytes, 37, seed)), expected, `seed ${seed}`);
    }
  });
}

test("fmp4: styp/sidx/moof group is one boundary at its first box", () => {
  const box = (type: string, bodyLength: number) => {
    const bytes = new Uint8Array(8 + bodyLength);
    new DataView(bytes.buffer).setUint32(0, bytes.length);
    bytes.set([...type].map((character) => character.charCodeAt(0)), 4);
    return bytes;
  };
  const stream = concat([box("styp", 4), box("sidx", 20), box("moof", 30), box("mdat", 50), box("moof", 30), box("mdat", 10)]);
  assert.deepEqual(scanAll("video/mp4", [stream]), [
    { kind: "media_start", offset: 0 },
    { kind: "media_start", offset: 12 + 28 + 38 + 58 },
  ]);
});

test("fmp4: largesize box header split across chunks is understood", () => {
  // mdat с 64-битным размером (size=1, largesize=8+8+100).
  const largeMdat = new Uint8Array(16 + 100);
  const view = new DataView(largeMdat.buffer);
  view.setUint32(0, 1);
  largeMdat.set([0x6d, 0x64, 0x61, 0x74], 4);
  view.setBigUint64(8, BigInt(largeMdat.length));
  const moof = new Uint8Array(16);
  new DataView(moof.buffer).setUint32(0, 16);
  moof.set([0x6d, 0x6f, 0x6f, 0x66], 4);
  const stream = concat([moof, largeMdat, moof]);
  for (let cut = 1; cut < stream.length; cut += 1) {
    assert.deepEqual(scanAll("video/mp4", splitAt(stream, [cut])), [
      { kind: "media_start", offset: 0 },
      { kind: "media_start", offset: 16 + largeMdat.length },
    ]);
  }
});

test("garbage, other containers and broken headers are reported as failures", () => {
  const mp3Frame = new Uint8Array([0xff, 0xfb, 0x90, 0x64, ...filler(200)]);
  assert.equal(scanAll("video/mp4", [mp3Frame]), null, "mp3 bytes under an mp4 mime");
  assert.equal(scanAll("video/webm", [VIDEO_MP4.bytes()]), null, "mp4 bytes under a webm mime");
  const tooSmallBox = new Uint8Array([0, 0, 0, 4, 0x6d, 0x6f, 0x6f, 0x66]);
  assert.equal(scanAll("video/mp4", [tooSmallBox]), null, "box smaller than its header");
  const sizeZero = new Uint8Array([0, 0, 0, 0, 0x6d, 0x64, 0x61, 0x74]);
  assert.equal(scanAll("video/mp4", [VIDEO_MP4.bytes().subarray(0, 733), sizeZero]), null, "size 0 box");
  const unknownSizeInfo = concat([ebmlElement(EBML_IDS.header, filler(4)), new Uint8Array([0x18, 0x53, 0x80, 0x67, 0xff]), new Uint8Array([0x15, 0x49, 0xa9, 0x66, 0xff])]);
  assert.equal(scanAll("video/webm", [unknownSizeInfo]), null, "unknown size outside Segment/Cluster");
  assert.equal(createSegmentScanner("audio/mpeg", 0), null, "mp3 is not scannable");
  assert.equal(createSegmentScanner("audio/aac", 0), null, "adts is not scannable");
});

test("after a failure the scanner keeps failing (no boundaries from a lost position)", () => {
  const scanner = createSegmentScanner("video/mp4", 0);
  assert.ok(scanner !== null);
  assert.equal(scanner.scan(new Uint8Array([0, 0, 0, 2, 0x41, 0x42, 0x43, 0x44])).kind, "failed");
  assert.equal(scanner.scan(VIDEO_MP4.bytes()).kind, "failed");
});

test("abort restarts parsing at the next append and keeps stream offsets", () => {
  const bytes = VIDEO_MP4.bytes();
  const scanner = createSegmentScanner("video/mp4", 0);
  assert.ok(scanner !== null);
  // Страница положила init и половину первого фрагмента, затем abort() и первый фрагмент заново.
  const first = scanner.scan(bytes.subarray(0, 1000));
  assert.deepEqual(first, {
    kind: "scanned",
    markers: [
      { kind: "init_start", offset: 0 },
      { kind: "init_end", offset: 733 },
      { kind: "media_start", offset: 733 },
    ],
  });
  scanner.restartAtNextAppend();
  const second = scanner.scan(bytes.subarray(733, 5714));
  assert.deepEqual(second, { kind: "scanned", markers: [{ kind: "media_start", offset: 1000 }] });
});

test("webm: a new EBML header mid-stream (quality switch) is a new init", () => {
  const first = liveWebm(2);
  const second = liveWebm(1);
  const stream = concat([first.bytes, second.bytes]);
  const markers = scanAll("video/webm", randomChunks(stream, 50, 3));
  const offset = first.bytes.length;
  assert.deepEqual(markers, [
    ...expectedMarkers(first),
    { kind: "init_start", offset },
    { kind: "init_end", offset: offset + second.initEnd },
    { kind: "media_start", offset: offset + (second.mediaStarts[0] ?? 0) },
  ]);
});
