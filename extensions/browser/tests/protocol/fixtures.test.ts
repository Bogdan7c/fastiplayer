// Общие фикстуры протокола (fixtures/manifest.json): TypeScript-сторона договора.
// Каждая фикстура: валидная - разбирается и пишется обратно БАЙТ В БАЙТ; невалидная -
// отвергается с ТОЧНЫМ видом причины. Расхождение с манифестом - ошибка TS-стороны.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DATA_FRAME_ERROR_KINDS,
  MAX_JSON_MESSAGE_BYTES,
  MESSAGE_ERROR_KINDS,
  decodeDataFrame,
  decodeExtensionControl,
  decodePageSession,
  decodePlayerControl,
  decodePlayerSession,
  encodeDataFrame,
  encodeExtensionControl,
  encodePageSession,
  encodePlayerControl,
  encodePlayerSession,
  type DataFrameHeader,
  type DecodeResult,
  type EncodeResult,
} from "../../src/protocol/index.ts";
import { pageSessionCodec } from "../../src/protocol/session/page.ts";
import { playerSessionCodec } from "../../src/protocol/session/player.ts";
import { type FixtureCase, loadManifest, readFixtureBytes } from "./fixtures-support.ts";

interface JsonSchemaOps {
  readonly decode: (input: Uint8Array) => DecodeResult<{ readonly type: string }>;
  readonly encode: (message: never) => EncodeResult<string>;
}

const JSON_SCHEMAS: Readonly<Record<string, JsonSchemaOps>> = {
  extension_control: { decode: decodeExtensionControl, encode: encodeExtensionControl },
  player_control: { decode: decodePlayerControl, encode: encodePlayerControl },
  page_session: { decode: decodePageSession, encode: encodePageSession },
  player_session: { decode: decodePlayerSession, encode: encodePlayerSession },
};

/** Поля заголовка рамки в форме фикстур; 64-битный номер больше 2^53-1 - строкой. */
function summarizeFrame(header: DataFrameHeader, payloadLength: number): Record<string, unknown> {
  if (header.kind === "source_buffer_bytes") {
    const sequence = header.sequence <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(header.sequence) : header.sequence.toString();
    return {
      kind: "source_buffer_bytes",
      generation: header.generation,
      source_buffer: header.source_buffer,
      sequence,
      payload_len: payloadLength,
    };
  }
  if (header.kind === "page_subtitle_bytes") {
    // Протокол 1.5: байты документа субтитров страницы.
    return { kind: "page_subtitle_bytes", track: header.track, offset: header.offset, payload_len: payloadLength };
  }
  return { kind: "range_bytes", request_id: header.request_id, offset: header.offset, payload_len: payloadLength };
}

function checkJsonCase(fixture: FixtureCase, bytes: Uint8Array): void {
  const schema = JSON_SCHEMAS[fixture.schema];
  assert.ok(schema, `неизвестная схема ${fixture.schema}`);
  const decoded = schema.decode(bytes);
  if (fixture.expect.result === "error") {
    assert.equal(decoded.ok, false, `${fixture.file}: должен быть отвергнут`);
    if (!decoded.ok) {
      assert.equal(decoded.error.kind, fixture.expect.kind, `${fixture.file}: ${decoded.error.detail}`);
    }
    return;
  }
  assert.equal(decoded.ok, true, `${fixture.file}: ${decoded.ok ? "" : `${decoded.error.kind} (${decoded.error.detail})`}`);
  if (decoded.ok) {
    const written = schema.encode(decoded.value as never);
    assert.equal(written.ok, true, `${fixture.file}: запись обратно`);
    if (written.ok) {
      assert.deepEqual(new TextEncoder().encode(written.value), bytes, `${fixture.file}: обратно не байт в байт`);
    }
  }
}

function checkDataFrameCase(fixture: FixtureCase, bytes: Uint8Array): void {
  const decoded = decodeDataFrame(bytes);
  if (fixture.expect.result === "error") {
    assert.equal(decoded.ok, false, `${fixture.file}: должен быть отвергнут`);
    if (!decoded.ok) {
      assert.equal(decoded.error.kind, fixture.expect.kind, fixture.file);
    }
    return;
  }
  assert.equal(decoded.ok, true, fixture.file);
  if (decoded.ok) {
    assert.deepEqual(summarizeFrame(decoded.value.header, decoded.value.payload.length), fixture.expect.frame, fixture.file);
    const written = encodeDataFrame(decoded.value.header, decoded.value.payload);
    assert.equal(written.ok, true, fixture.file);
    if (written.ok) {
      assert.deepEqual(written.value, bytes, `${fixture.file}: рамка обратно не байт в байт`);
    }
  }
}

const manifest = loadManifest();

test("манифест содержит ожидаемый набор случаев", () => {
  assert.ok(manifest.cases.length >= 97, `случаев: ${manifest.cases.length}`);
  assert.equal(manifest.streams.length, 6);
});

// Один подтест на фикстуру: при падении видно, какая именно.
for (const fixture of manifest.cases) {
  test(`фикстура ${fixture.file}`, () => {
    const bytes = readFixtureBytes(fixture.file);
    if (fixture.schema === "data_frame") {
      checkDataFrameCase(fixture, bytes);
    } else {
      checkJsonCase(fixture, bytes);
    }
  });
}

test("все виды причин из фикстур существуют в словаре TypeScript", () => {
  const known = new Set<string>([...MESSAGE_ERROR_KINDS, ...DATA_FRAME_ERROR_KINDS]);
  for (const fixture of manifest.cases) {
    if (fixture.expect.result === "error") {
      assert.ok(known.has(fixture.expect.kind), `${fixture.file}: вид ${fixture.expect.kind}`);
    }
  }
});

test("у каждой схемы валидные фикстуры покрывают все известные типы сообщений", () => {
  // Совпадение множеств «типы в фикстурах» и «типы, которые знает кодек» ловит и забытый вариант, и лишний.
  const coveredTypes = (schema: string): string[] =>
    manifest.cases
      .filter((fixture) => fixture.schema === schema && fixture.expect.result === "ok")
      .map((fixture) => (JSON.parse(new TextDecoder().decode(readFixtureBytes(fixture.file))) as { type: string }).type)
      .sort();
  const expectedTypes: Readonly<Record<string, readonly string[]>> = {
    extension_control: ["hello", "open_session"],
    player_control: ["hello_accepted", "refused", "session_granted"],
  };
  for (const [schema, expected] of Object.entries(expectedTypes)) {
    assert.deepEqual([...new Set(coveredTypes(schema))], [...expected], schema);
  }
  // Сверка с полным списком типов кодека: фикстура на каждый тип и ни одного лишнего.
  assert.deepEqual([...new Set(coveredTypes("page_session"))].sort(), [...pageSessionCodec.knownTags].sort());
  assert.deepEqual([...new Set(coveredTypes("player_session"))].sort(), [...playerSessionCodec.knownTags].sort());
  assert.equal(new Set(coveredTypes("page_session")).size, 22);
  assert.equal(new Set(coveredTypes("player_session")).size, 16);
});

// ---------------------------------------------------------------- предел размера (каждая сторона собирает сама)

const HELLO_TEXT = '{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}';

/** Валидный hello, добитый пробелами внутри объекта до заданной длины. */
function paddedHello(totalBytes: number): Uint8Array {
  const body = HELLO_TEXT.slice(0, -1);
  return new TextEncoder().encode(body + " ".repeat(totalBytes - body.length - 1) + "}");
}

test("сообщение ровно в лимит принимается, на байт длиннее - too_large", () => {
  const atLimit = paddedHello(MAX_JSON_MESSAGE_BYTES);
  assert.equal(atLimit.length, MAX_JSON_MESSAGE_BYTES);
  const accepted = decodeExtensionControl(atLimit);
  assert.equal(accepted.ok, true);
  const overLimit = paddedHello(MAX_JSON_MESSAGE_BYTES + 1);
  const rejected = decodeExtensionControl(overLimit);
  assert.equal(rejected.ok, false);
  if (!rejected.ok) assert.equal(rejected.error.kind, "too_large");
  // Строкой вход режется по байтам UTF-8, а не по числу символов.
  const asText = decodeExtensionControl(new TextDecoder().decode(overLimit));
  assert.equal(asText.ok === false && asText.error.kind, "too_large");
});

test("самое крупное честное меню (256 вариантов по 256 байт подписи) пишется и помещается в лимит", () => {
  const menuOptions = Array.from({ length: 256 }, (_, index) => ({
    id: `v-${index}`,
    label: "я".repeat(128), // 256 байт UTF-8 на подпись - предел
    selection: "available" as const,
  }));
  const written = encodePageSession({ type: "variant_menu_changed", menu: { kind: "audio", options: menuOptions } });
  assert.equal(written.ok, true);
  if (written.ok) {
    const size = new TextEncoder().encode(written.value).length;
    assert.ok(size > 60 * 1024 && size <= MAX_JSON_MESSAGE_BYTES, `размер ${size}`);
    // И читается обратно тем же разбором.
    assert.equal(decodePageSession(written.value).ok, true);
  }
});
