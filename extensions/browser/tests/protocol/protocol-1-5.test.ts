// Договоры протокола 1.5 (сессия 13b1, решения 270-274 - субтитры страницы), которых не
// покрывает общий проход по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - одно новое сообщение `subtitle_track_offered` (fixtures/message-versions.json) и новый вид
//   рамки 3 (`dataFrameIntroducedIn`);
// - сообщение пишется ровно задокументированным JSON, рамка - задокументированными байтами;
// - каждый шаг общих случаев сторожа (fixtures/page-subtitle-gate.json) разбирается кодеками
//   TypeScript и пишется обратно байт в байт (сам сторож - у плеера, Rust);
// - нарезка документа `splitPageSubtitleDocument`: заголовок с точной длиной и куски <= 1 МиБ
//   подряд с 0, без копий, собираются в документ байт в байт.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_5.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  MAX_DATA_FRAME_PAYLOAD_BYTES,
  MAX_PAGE_SUBTITLE_DOCUMENT_BYTES,
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_4,
  PROTOCOL_VERSION_1_5,
  dataFrameIntroducedIn,
  decodeDataFrame,
  decodePageSession,
  encodeDataFrame,
  encodePageSession,
  negotiateProtocolVersion,
  pageSessionIntroducedIn,
  pageSessionRequiredVersion,
  splitPageSubtitleDocument,
  versionIncludes,
  type PageSessionMessage,
  type PageSubtitleOfferFields,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

function readJson<T>(name: string): T {
  return JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as T;
}

/** Шаг общего случая сторожа: текстовая рамка или двоичная рамка в hex. */
type GateStep = { readonly text: string } | { readonly binary_hex: string };

interface GateCase {
  readonly name: string;
  readonly agreed: string;
  readonly steps: readonly GateStep[];
}

/** Поля заголовка дорожки для нарезки (длину считает помощник). */
const UKRAINIAN_TRACK: PageSubtitleOfferFields = {
  track: 3,
  language: { kind: "known", tag: "uk" },
  label: { kind: "text", text: "Українська" },
  format_hint: "json3",
  origin: "page_download",
  page_display: "showing",
};

function hexToBytes(hex: string): Uint8Array {
  return Uint8Array.from(hex.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
}

test("в 1.5 ровно одно новое сообщение - subtitle_track_offered, и только у страницы", () => {
  const table = readJson<Record<string, Record<string, string>>>("message-versions.json");
  const newer = (side: string): string[] =>
    Object.entries(table[side] ?? {})
      .filter(([, version]) => version === "1.5")
      .map(([type]) => type);
  assert.deepEqual(newer("page_session"), ["subtitle_track_offered"]);
  assert.deepEqual(newer("player_session"), []);
  assert.deepEqual(pageSessionIntroducedIn("subtitle_track_offered"), PROTOCOL_VERSION_1_5);
});

test("рамка вида 3 - из 1.5, прежние виды - из 1.0; при общей 1.4 субтитры не шлются", () => {
  assert.deepEqual(dataFrameIntroducedIn("page_subtitle_bytes"), PROTOCOL_VERSION_1_5);
  assert.deepEqual(dataFrameIntroducedIn("range_bytes"), PROTOCOL_VERSION_1_0);
  assert.deepEqual(dataFrameIntroducedIn("source_buffer_bytes"), PROTOCOL_VERSION_1_0);
  const olderPlayer = negotiateProtocolVersion(PROTOCOL_VERSION_1_4, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(olderPlayer, { ok: true, version: PROTOCOL_VERSION_1_4 });
  assert.equal(olderPlayer.ok && versionIncludes(olderPlayer.version, dataFrameIntroducedIn("page_subtitle_bytes")), false);
  const offer: PageSessionMessage = { type: "subtitle_track_offered", ...UKRAINIAN_TRACK, byte_length: 10 };
  assert.equal(olderPlayer.ok && versionIncludes(olderPlayer.version, pageSessionRequiredVersion(offer)), false);
});

test("заголовок пишется ровно задокументированным JSON и читается обратно", () => {
  const cases: ReadonlyArray<readonly [PageSessionMessage, string]> = [
    [
      { type: "subtitle_track_offered", ...UKRAINIAN_TRACK, track: 7, byte_length: 4096 },
      '{"type":"subtitle_track_offered","track":7,"language":{"kind":"known","tag":"uk"},' +
        '"label":{"kind":"text","text":"Українська"},"format_hint":"json3","origin":"page_download",' +
        '"page_display":"showing","byte_length":4096}',
    ],
    [
      {
        type: "subtitle_track_offered",
        track: 2,
        language: { kind: "unknown" },
        label: { kind: "none" },
        format_hint: "web_vtt",
        origin: "text_track_cues",
        page_display: "not_showing",
        byte_length: 1,
      },
      '{"type":"subtitle_track_offered","track":2,"language":{"kind":"unknown"},"label":{"kind":"none"},' +
        '"format_hint":"web_vtt","origin":"text_track_cues","page_display":"not_showing","byte_length":1}',
    ],
  ];
  for (const [message, json] of cases) {
    const written = encodePageSession(message);
    assert.ok(written.ok, json);
    assert.equal(written.value, json);
    assert.deepEqual(decodePageSession(json), { ok: true, value: message });
  }
});

test("тег языка: форма BCP-47, дефис только между частями; запись отвергает то же, что разбор", () => {
  for (const tag of ["en", "pt-BR", "zh-Hant-TW", "es-419", "EN-us"]) {
    const written = encodePageSession({ type: "subtitle_track_offered", ...UKRAINIAN_TRACK, language: { kind: "known", tag }, byte_length: 1 });
    assert.ok(written.ok, tag);
  }
  for (const [tag, kind] of [
    ["", "text_empty"],
    ["a".repeat(36), "text_too_long"],
    ["en_US", "text_forbidden_character"],
    ["-en", "text_forbidden_character"],
    ["en-", "text_forbidden_character"],
    ["en--US", "text_forbidden_character"],
  ] as const) {
    const written = encodePageSession({ type: "subtitle_track_offered", ...UKRAINIAN_TRACK, language: { kind: "known", tag }, byte_length: 1 });
    assert.equal(written.ok ? "ok" : written.error.kind, kind, tag);
  }
});

test("рамка вида 3 имеет задокументированные байты и читается обратно", () => {
  const frame = encodeDataFrame({ kind: "page_subtitle_bytes", track: 0x0102_0304, offset: 0x0506_0708_090a }, new TextEncoder().encode("WEBVTT"));
  assert.ok(frame.ok);
  const expected = new Uint8Array(24 + 6);
  expected.set([3, 0, 0, 0, 0x04, 0x03, 0x02, 0x01, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0, 0]);
  expected.set(new TextEncoder().encode("WEBVTT"), 24);
  assert.deepEqual(frame.value, expected);
  const parsed = decodeDataFrame(frame.value);
  assert.ok(parsed.ok);
  assert.deepEqual(parsed.value.header, { kind: "page_subtitle_bytes", track: 0x0102_0304, offset: 0x0506_0708_090a });
  // Дорожка 0 не пишется: то же правило, что у разбора.
  const zero = encodeDataFrame({ kind: "page_subtitle_bytes", track: 0, offset: 0 }, new Uint8Array([1]));
  assert.equal(zero.ok ? "ok" : zero.error.kind, "number_out_of_range");
});

test("каждый шаг общих случаев сторожа разбирается кодеками TypeScript и пишется обратно байт в байт", () => {
  const { cases } = readJson<{ cases: GateCase[] }>("page-subtitle-gate.json");
  assert.ok(cases.length >= 15, "случаев сторожа меньше, чем правил");
  let subtitleFrames = 0;
  for (const gateCase of cases) {
    for (const step of gateCase.steps) {
      if ("text" in step) {
        const decoded = decodePageSession(step.text);
        assert.ok(decoded.ok, `${gateCase.name}: ${decoded.ok ? "" : decoded.error.kind}`);
        assert.deepEqual(encodePageSession(decoded.value), { ok: true, value: step.text }, gateCase.name);
        continue;
      }
      const bytes = hexToBytes(step.binary_hex);
      const decoded = decodeDataFrame(bytes);
      assert.ok(decoded.ok, `${gateCase.name}: ${decoded.ok ? "" : decoded.error.kind}`);
      assert.deepEqual(encodeDataFrame(decoded.value.header, decoded.value.payload), { ok: true, value: bytes }, gateCase.name);
      if (decoded.value.header.kind === "page_subtitle_bytes") subtitleFrames += 1;
    }
  }
  assert.ok(subtitleFrames > 0, "в случаях сторожа есть рамки вида 3");
});

test("splitPageSubtitleDocument: заголовок с точной длиной, куски <= 1 МиБ подряд с 0, без копий", () => {
  // 2,5 МиБ: куски 1 МиБ, 1 МиБ и 0,5 МиБ.
  const document = new Uint8Array((MAX_DATA_FRAME_PAYLOAD_BYTES * 5) / 2).map((_, index) => index % 251);
  const split = splitPageSubtitleDocument(UKRAINIAN_TRACK, document);
  assert.ok(split.ok);
  assert.deepEqual(split.value.offer, { type: "subtitle_track_offered", ...UKRAINIAN_TRACK, byte_length: document.length });
  assert.ok(encodePageSession(split.value.offer).ok, "заголовок проходит строгую запись");
  assert.deepEqual(
    split.value.pieces.map((piece) => [piece.header.offset, piece.payload.length]),
    [
      [0, MAX_DATA_FRAME_PAYLOAD_BYTES],
      [MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES],
      [2 * MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES / 2],
    ],
  );
  const reassembled = new Uint8Array(document.length);
  for (const piece of split.value.pieces) {
    assert.equal(piece.payload.buffer, document.buffer, "кусок - подмассив документа, а не копия");
    assert.equal(piece.header.track, UKRAINIAN_TRACK.track);
    // Кусок проходит путь провода: рамка собирается и разбирается тем же видом 3.
    const frame = encodeDataFrame(piece.header, piece.payload);
    assert.ok(frame.ok);
    const parsed = decodeDataFrame(frame.value);
    assert.ok(parsed.ok);
    assert.deepEqual(parsed.value.header, piece.header);
    reassembled.set(parsed.value.payload, piece.header.offset);
  }
  assert.deepEqual(reassembled, document);
});

test("splitPageSubtitleDocument: пустой, слишком длинный документ и дорожка 0 - отказ с причиной", () => {
  const empty = splitPageSubtitleDocument(UKRAINIAN_TRACK, new Uint8Array(0));
  assert.deepEqual(empty, { ok: false, error: { kind: "number_out_of_range", detail: "byte_length" } });
  const tooLarge = splitPageSubtitleDocument(UKRAINIAN_TRACK, new Uint8Array(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES + 1));
  assert.deepEqual(tooLarge, { ok: false, error: { kind: "number_out_of_range", detail: "byte_length" } });
  const atLimit = splitPageSubtitleDocument(UKRAINIAN_TRACK, new Uint8Array(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES));
  assert.ok(atLimit.ok);
  assert.equal(atLimit.value.pieces.length, MAX_PAGE_SUBTITLE_DOCUMENT_BYTES / MAX_DATA_FRAME_PAYLOAD_BYTES);
  const zeroTrack = splitPageSubtitleDocument({ ...UKRAINIAN_TRACK, track: 0 }, new Uint8Array([1]));
  assert.deepEqual(zeroTrack, { ok: false, error: { kind: "number_out_of_range", detail: "track" } });
  // Однобайтовый документ - один кусок со смещением 0; ArrayBuffer тоже без копии.
  const buffer = new Uint8Array([0x57]).buffer;
  const single = splitPageSubtitleDocument(UKRAINIAN_TRACK, buffer);
  assert.ok(single.ok);
  assert.equal(single.value.pieces.length, 1);
  assert.equal(single.value.pieces[0]?.payload.buffer, buffer);
  assert.equal(single.value.offer.byte_length, 1);
});
