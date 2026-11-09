// Пограничник событий субтитров страницы (13b2): валидные события проходят тем же видом байт,
// мусор — лишнее поле, пустые байты, SharedArrayBuffer, > 8 МиБ, ключ 0, длинная строка, не тот тип — отказ.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "../../src/protocol/limits.ts";
import { parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { MAX_PAGE_SUBTITLE_LABEL_CHARS, MAX_PAGE_SUBTITLE_LANGUAGE_CHARS } from "../../src/shared/page-subtitle-contract.ts";

const NAMING = { language: "en", label: "English", auto_generated: false };
const document = (overrides: Record<string, unknown> = {}) => ({
  kind: "subtitle_document",
  key: 1,
  naming: NAMING,
  format_hint: "web_vtt",
  origin: "page_download",
  display: "showing",
  bytes: new TextEncoder().encode("WEBVTT\n\n"),
  ...overrides,
});
const trackSource = (overrides: Record<string, unknown> = {}) => ({
  kind: "subtitle_track_source",
  key: 2,
  naming: NAMING,
  display: "not_showing",
  url: "https://cdn.example/subs/en.vtt",
  referrer_origin: "https://site.example",
  referrer_policy: "origin",
  ...overrides,
});

describe("subtitle_document на границе", () => {
  it("валидный документ проходит, байты — тот же вид, поля сохранены", () => {
    const input = document();
    const parsed = parsePageEvent(input);
    assert.ok(parsed !== undefined && parsed.kind === "subtitle_document");
    assert.equal(parsed.bytes, input.bytes, "тот же Uint8Array, без копии");
    assert.deepEqual({ ...parsed, bytes: undefined }, { ...input, bytes: undefined });
  });

  it("naming с пустыми языком и подписью (null) проходит; origin text_track_cues — тоже", () => {
    const parsed = parsePageEvent(document({ naming: { language: null, label: null, auto_generated: true }, origin: "text_track_cues", display: "not_showing" }));
    assert.ok(parsed !== undefined && parsed.kind === "subtitle_document");
    assert.deepEqual(parsed.naming, { language: null, label: null, auto_generated: true });
  });

  it("отказ: лишнее поле, ключ 0 и вне u32, неизвестные origin/display/format", () => {
    const rejected: Record<string, unknown>[] = [
      { extra: 1 },
      { key: 0 },
      { key: 2 ** 32 },
      { key: 1.5 },
      { origin: "track_element" },
      { display: "hidden" },
      { format_hint: "ass" },
    ];
    for (const override of rejected) assert.equal(parsePageEvent(document(override)), undefined, JSON.stringify(override));
  });

  it("отказ: пустые байты, SharedArrayBuffer, больше 8 МиБ, ArrayBuffer вместо Uint8Array, обычный массив", () => {
    assert.equal(parsePageEvent(document({ bytes: new Uint8Array(0) })), undefined, "пустые байты");
    assert.equal(parsePageEvent(document({ bytes: new Uint8Array(new SharedArrayBuffer(8)) })), undefined, "SharedArrayBuffer");
    assert.equal(parsePageEvent(document({ bytes: new Uint8Array(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES + 1) })), undefined, "больше потолка");
    assert.ok(parsePageEvent(document({ bytes: new Uint8Array(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES) })) !== undefined, "ровно потолок проходит");
    assert.equal(parsePageEvent(document({ bytes: new ArrayBuffer(8) })), undefined, "ArrayBuffer вместо Uint8Array");
    assert.equal(parsePageEvent(document({ bytes: [87, 69] })), undefined, "обычный массив");
  });

  it("отказ: naming не той формы, длинные язык и подпись, пустая строка вместо null", () => {
    const rejected: unknown[] = [
      { ...NAMING, extra: true },
      { language: "x".repeat(MAX_PAGE_SUBTITLE_LANGUAGE_CHARS + 1), label: null, auto_generated: false },
      { language: "en", label: "y".repeat(MAX_PAGE_SUBTITLE_LABEL_CHARS + 1), auto_generated: false },
      { language: "", label: null, auto_generated: false },
      { language: "en", label: null, auto_generated: "yes" },
      null,
      "English",
    ];
    for (const naming of rejected) assert.equal(parsePageEvent(document({ naming })), undefined, JSON.stringify(naming));
  });
});

describe("subtitle_track_source на границе", () => {
  it("валидное событие проходит со всеми полями", () => {
    const input = trackSource();
    assert.deepEqual(parsePageEvent(input), input);
  });

  it("отказ: адрес не http(s), с логином, неканонический, пустой; лишнее поле; ключ 0", () => {
    const rejected: Record<string, unknown>[] = [
      { url: "blob:https://site.example/1" },
      { url: "data:text/vtt,WEBVTT" },
      { url: "https://user:pw@cdn.example/a.vtt" },
      { url: "HTTPS://CDN.EXAMPLE/a.vtt" },
      { url: "" },
      { extra: 1 },
      { key: 0 },
      { referrer_origin: "https://site.example/path" },
      { referrer_policy: "sometimes" },
    ];
    for (const override of rejected) assert.equal(parsePageEvent(trackSource(override)), undefined, JSON.stringify(override));
  });

  it("referrer_origin: null допустим только вместе с no-referrer-логикой пограничника (проходит как null)", () => {
    const parsed = parsePageEvent(trackSource({ referrer_origin: null, referrer_policy: "no-referrer" }));
    assert.ok(parsed !== undefined && parsed.kind === "subtitle_track_source");
    assert.equal(parsed.referrer_origin, null);
  });
});
