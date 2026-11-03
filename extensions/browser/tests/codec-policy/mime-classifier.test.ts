// Разбор MIME-строк: реальные строки YouTube/Twitch/hls.js -> заказанные семейства видео.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { classifyMimeVideo, isHlsMimeType, refusedFamilies, type MimeVerdict } from "../../src/page/codec-policy/mime-classifier.ts";

const video = (...families: string[]): MimeVerdict => ({ kind: "video", families } as MimeVerdict);

describe("classifyMimeVideo: какие видео-кодеки заказывает строка", () => {
  const table: ReadonlyArray<readonly [string, MimeVerdict]> = [
    ['video/mp4; codecs="avc1.64001f, mp4a.40.2"', video("h264")],
    ['video/mp4; codecs="avc1.4d401f"', video("h264")],
    ['video/mp4; codecs="avc3.640028"', video("h264")],
    ['video/webm; codecs="vp9"', video("vp9")],
    ["video/webm; codecs=vp9", video("vp9")],
    ['video/webm; codecs="vp09.00.10.08"', video("vp9")],
    ['video/mp4; codecs="vp09.00.40.08"', video("vp9")],
    ['video/mp4; codecs="av01.0.05M.08"', video("av1")],
    ['video/mp4; codecs="av01.0.08M.10, mp4a.40.2"', video("av1")],
    ['video/mp4; codecs="hev1.1.6.L93.B0"', video("hevc")],
    ['video/mp4; codecs="hvc1.2.4.L153.B0"', video("hevc")],
    ['video/webm; codecs="vp8"', video("vp8")],
    ['video/webm; codecs="vp8, vorbis"', video("vp8")],
    ["video/ogg; codecs=theora", video("other_video")],
    // Заказаны два видео сразу.
    ['video/mp4; codecs="avc1.64001f, av01.0.05M.08"', video("h264", "av1")],
    // Регистр и пробелы.
    ['  VIDEO/MP4 ;  CODECS = "AVC1.64001F , MP4A.40.2" ', video("h264")],
    ["video/mp4;codecs='avc1.42E01E'", video("h264")],
    // Голый WebM — семейство VP9; голый MP4 может быть H.264 — решает браузер.
    ["video/webm", video("vp9")],
    ["video/mp4", { kind: "unknown" }],
    ["video/quicktime", { kind: "unknown" }],
    // Только звук.
    ['audio/mp4; codecs="mp4a.40.2"', { kind: "no_video" }],
    ['audio/webm; codecs="opus"', { kind: "no_video" }],
    ["audio/mpeg", { kind: "no_video" }],
    ['video/mp4; codecs="mp4a.40.2"', { kind: "no_video" }],
    // Мусор — «не знаю».
    ["", { kind: "unknown" }],
    ["garbage", { kind: "unknown" }],
    ["video/mp4; codecs=", { kind: "unknown" }],
    ['video/mp4; codecs="zzzz.1, yyyy"', { kind: "unknown" }],
    ['video/mp4; codecs="mp4a.40.2, zzzz"', { kind: "unknown" }],
    ["/;;;===", { kind: "unknown" }],
    ["x".repeat(5000), { kind: "unknown" }],
  ];
  for (const [mime, expected] of table) {
    it(`${JSON.stringify(mime.length > 60 ? `${mime.slice(0, 40)}…(${mime.length})` : mime)}`, () => {
      assert.deepEqual(classifyMimeVideo(mime), expected);
    });
  }
});

describe("isHlsMimeType", () => {
  it("все четыре типа, в любом регистре и с параметрами", () => {
    for (const mime of [
      "application/vnd.apple.mpegurl",
      "application/x-mpegURL",
      "AUDIO/MPEGURL",
      "audio/x-mpegurl",
      'application/vnd.apple.mpegurl; codecs="avc1.4d401f, mp4a.40.2"',
      "  application/x-mpegurl ; charset=utf-8",
    ]) {
      assert.equal(isHlsMimeType(mime), true, mime);
    }
  });

  it("обычные типы — не HLS", () => {
    for (const mime of ["video/mp4", "audio/mpeg", "application/dash+xml", "", "application/mpegurl-not"]) {
      assert.equal(isHlsMimeType(mime), false, mime);
    }
  });
});

describe("refusedFamilies", () => {
  const everythingButH264 = ["av1", "vp9", "hevc", "vp8"] as const;
  it("отвергает названные семейства и не трогает остальное, звук и неопознанное", () => {
    assert.deepEqual(refusedFamilies('video/mp4; codecs="av01.0.05M.08"', everythingButH264), ["av1"]);
    assert.deepEqual(refusedFamilies('video/mp4; codecs="avc1.64001f, mp4a.40.2"', everythingButH264), []);
    assert.deepEqual(refusedFamilies('video/webm; codecs="vp9"', ["av1"]), []);
    assert.deepEqual(refusedFamilies('video/webm; codecs="vp8"', ["av1"]), []);
    assert.deepEqual(refusedFamilies('video/webm; codecs="vp8"', everythingButH264), ["vp8"]);
    assert.deepEqual(refusedFamilies('video/mp4; codecs="avc1.64001f, av01.0.05M.08"', ["av1"]), ["av1"]);
    assert.deepEqual(refusedFamilies("video/ogg; codecs=theora", everythingButH264), [], "other_video политика не называет");
    assert.deepEqual(refusedFamilies('audio/mp4; codecs="mp4a.40.2"', everythingButH264), []);
    assert.deepEqual(refusedFamilies("video/mp4", everythingButH264), []);
  });
});
