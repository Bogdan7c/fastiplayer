// Опознание документа субтитров по содержимому (13b2): форматы, кусочек HLS и всё остальное.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { sniffSubtitleDocument } from "../../src/shared/subtitle-document-sniff.ts";

const sniff = (text: string): unknown => sniffSubtitleDocument(new TextEncoder().encode(text));

describe("sniffSubtitleDocument", () => {
  it("опознаёт WebVTT, SRT, TTML, srv3 YouTube и json3 YouTube", () => {
    assert.deepEqual(sniff("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n"), { kind: "document", format: "web_vtt" });
    assert.deepEqual(sniff("﻿WEBVTT - title\n\n00:01.000 --> 00:02.000\nHi\n"), { kind: "document", format: "web_vtt" }, "BOM и подпись после WEBVTT");
    assert.deepEqual(sniff("1\r\n00:00:01,000 --> 00:00:02,000\r\nHi\r\n"), { kind: "document", format: "srt" });
    assert.deepEqual(sniff('<?xml version="1.0" encoding="utf-8"?><tt xmlns="http://www.w3.org/ns/ttml"><body/></tt>'), { kind: "document", format: "ttml" });
    assert.deepEqual(sniff('<?xml version="1.0"?><timedtext format="3"><body><p t="0" d="1000">Hi</p></body></timedtext>'), { kind: "document", format: "srv3" });
    assert.deepEqual(sniff('{"wireMagic":"pb3","events":[{"tStartMs":0,"dDurationMs":1000,"segs":[{"utf8":"Hi"}]}]}'), { kind: "document", format: "json3" });
  });

  it("кусок HLS (WebVTT с X-TIMESTAMP-MAP) — не дорожка", () => {
    assert.deepEqual(sniff("WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:00:01.000 --> 00:00:02.000\nHi\n"), { kind: "hls_segment" });
  });

  it("всё остальное — не субтитры: чужой JSON, HTML, плейлист, двоичное, пустое", () => {
    assert.deepEqual(sniff('{"user":"x","items":[1,2,3]}'), { kind: "not_subtitles" });
    assert.deepEqual(sniff("<!doctype html><html></html>"), { kind: "not_subtitles" });
    assert.deepEqual(sniff("#EXTM3U\n#EXT-X-VERSION:3\n"), { kind: "not_subtitles" });
    assert.deepEqual(sniffSubtitleDocument(new Uint8Array([0x00, 0x00, 0x00, 0x18, 0x66, 0x74, 0x79, 0x70])), { kind: "not_subtitles" });
    assert.deepEqual(sniffSubtitleDocument(new Uint8Array(0)), { kind: "not_subtitles" });
  });
});
