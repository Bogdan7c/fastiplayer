// Фильтр ответа до чтения тела (13b2): что читаем, что не читаем никогда.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { addressLooksLikeSubtitles, decideSubtitleResponse, MAX_SUBTITLE_RESPONSE_BYTES, type SubtitleResponseCandidate } from "../../../src/page/page-subtitles/response-filter.ts";

/** Ответ 200 с заданными адресом и типом; остальное — как у обычного ответа без размера. */
function response(url: string, contentType: string | null, overrides: Partial<SubtitleResponseCandidate> = {}): SubtitleResponseCandidate {
  return { url, status: 200, responseType: "basic", contentType, contentLength: null, ...overrides };
}

const reads = (candidate: SubtitleResponseCandidate): boolean => decideSubtitleResponse(candidate).read;

describe("фильтр ответа субтитров: что читаем", () => {
  it("типы субтитров читаются по любому адресу", () => {
    for (const type of ["text/vtt", "text/vtt; charset=utf-8", "TEXT/VTT", "application/x-subrip", "text/srt", "application/ttml+xml", "application/ttaf+xml"]) {
      assert.equal(reads(response("https://cdn.example/media/12345", type)), true, type);
    }
  });

  it("двусмысленный тип читается только при явно субтитровом адресе", () => {
    assert.equal(reads(response("https://cdn.example/subs/en.vtt", "text/plain")), true, "расширение .vtt");
    assert.equal(reads(response("https://cdn.example/subs/en.SRT?x=1", "application/octet-stream")), true, "расширение .srt в верхнем регистре, двоичный тип");
    assert.equal(reads(response("https://cdn.example/subs/en.dfxp", null)), true, "тип не назван, расширение .dfxp");
    assert.equal(reads(response("https://www.youtube.com/api/timedtext?v=abc&lang=en&fmt=json3", "application/json; charset=UTF-8")), true, "timedtext + json3");
    assert.equal(reads(response("https://api.example/v1/captions/42.json", "application/json")), true, "путь с captions");
    assert.equal(reads(response("https://api.example/v1/play?id=9&fmt=srv3", "text/xml")), true, "fmt=srv3");
  });

  it("любой JSON и XML сайта без субтитрового адреса не читается", () => {
    assert.deepEqual(decideSubtitleResponse(response("https://api.example/v1/me.json", "application/json")), { read: false, reason: "ambiguous_type_and_plain_address" });
    assert.equal(reads(response("https://api.example/feed.xml", "text/xml")), false);
    assert.equal(reads(response("https://api.example/data", null)), false, "тип не назван, адрес обычный");
    assert.equal(reads(response("https://api.example/data?fmt=json", "application/json")), false, "fmt=json — не субтитровый формат");
  });
});

describe("фильтр ответа субтитров: что не читаем никогда", () => {
  it("видео, аудио, картинки, шрифты и манифесты не читаются, даже с субтитровым адресом", () => {
    const binaryTypes = ["video/mp4", "audio/mpeg", "image/png", "font/woff2", "application/vnd.apple.mpegurl", "application/x-mpegURL", "application/dash+xml", "application/zip", "application/pdf"];
    for (const type of binaryTypes) {
      assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/subtitles/en.vtt", type)), { read: false, reason: "never_readable" }, type);
    }
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/live/master.m3u8", "text/plain")), { read: false, reason: "never_readable" }, "манифест по расширению");
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/captions/stream.mpd", "text/vtt")), { read: false, reason: "never_readable" });
  });

  it("чужие текстовые типы (html, css, скрипт) не читаются", () => {
    for (const type of ["text/html", "text/css", "application/javascript", "text/javascript"]) {
      assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/captions/en.vtt", type)), { read: false, reason: "foreign_type" }, type);
    }
  });

  it("неуспешные, частичные и непрозрачные ответы не читаются", () => {
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/a.vtt", "text/vtt", { status: 404 })), { read: false, reason: "bad_status" });
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/a.vtt", "text/vtt", { status: 206 })), { read: false, reason: "bad_status" }, "кусок — не весь документ");
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/a.vtt", "text/vtt", { status: 0 })), { read: false, reason: "bad_status" });
    for (const type of ["opaque", "opaqueredirect", "error"]) {
      assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/a.vtt", "text/vtt", { responseType: type })), { read: false, reason: "opaque_response" }, type);
    }
  });

  it("адрес не http(s) или пустой (ответ service worker) не читается", () => {
    assert.deepEqual(decideSubtitleResponse(response("", "text/vtt")), { read: false, reason: "bad_address" });
    assert.deepEqual(decideSubtitleResponse(response("blob:https://site.example/1", "text/vtt")), { read: false, reason: "bad_address" });
    assert.deepEqual(decideSubtitleResponse(response("data:text/vtt,WEBVTT", "text/vtt")), { read: false, reason: "bad_address" });
  });

  it("размер: длиннее потолка и пустое тело не читаются, неизвестный размер проходит", () => {
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/a.vtt", "text/vtt", { contentLength: String(MAX_SUBTITLE_RESPONSE_BYTES + 1) })), { read: false, reason: "too_large" });
    assert.equal(reads(response("https://cdn.example/a.vtt", "text/vtt", { contentLength: String(MAX_SUBTITLE_RESPONSE_BYTES) })), true, "ровно потолок читается");
    assert.deepEqual(decideSubtitleResponse(response("https://cdn.example/a.vtt", "text/vtt", { contentLength: "0" })), { read: false, reason: "empty_body" });
    assert.equal(reads(response("https://cdn.example/a.vtt", "text/vtt", { contentLength: null })), true, "размера нет — потолок соблюдает читатель потока");
    assert.equal(reads(response("https://cdn.example/a.vtt", "text/vtt", { contentLength: "garbage" })), true, "мусор в заголовке не считается размером");
  });
});

describe("адрес «про субтитры»", () => {
  it("узнаёт расширения и метки API, не путая обычные адреса", () => {
    assert.equal(addressLooksLikeSubtitles("https://x.example/a/b.webvtt"), true);
    assert.equal(addressLooksLikeSubtitles("https://x.example/Subtitle/42"), true);
    assert.equal(addressLooksLikeSubtitles("https://x.example/api/timedtext?fmt=ttml"), true);
    assert.equal(addressLooksLikeSubtitles("https://x.example/api/me"), false);
    assert.equal(addressLooksLikeSubtitles("/relative/en.vtt"), false, "относительный адрес не принимаем");
  });
});
