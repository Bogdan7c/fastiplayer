// Общие фикстуры протокола (fixtures/manifest.json): TypeScript-сторона договора.
// Каждая фикстура: валидная - разбирается и пишется обратно БАЙТ В БАЙТ; невалидная -
// отвергается с ТОЧНЫМ видом причины. Расхождение с манифестом - ошибка TS-стороны.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
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
  pageSessionRequiredVersion,
  playerSessionRequiredVersion,
  type DataFrameHeader,
  type DecodeResult,
  type EncodeResult,
  type ProtocolVersion,
} from "../../src/protocol/index.ts";
import { pageSessionCodec } from "../../src/protocol/session/page.ts";
import { playerSessionCodec } from "../../src/protocol/session/player.ts";
import { FIXTURES_ROOT, type FixtureCase, loadManifest, readFixtureBytes } from "./fixtures-support.ts";

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
  assert.equal(new Set(coveredTypes("page_session")).size, 27);
  assert.equal(new Set(coveredTypes("player_session")).size, 19);
});

// ---------------------------------------------------------------- версия каждой валидной фикстуры сеанса

type SideTable = Readonly<Record<"page_session" | "player_session", Readonly<Record<string, string>>>>;

const readTable = <T>(name: string): T => JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as T;

/** Версия из текста таблицы - ЧИСЛАМИ: "1.10" как строка меньше "1.9". */
function versionOf(text: string): ProtocolVersion {
  const [major, minor] = text.split(".").map(Number);
  assert.ok(Number.isInteger(major) && Number.isInteger(minor), `версия ${text}`);
  return { major: major ?? Number.NaN, minor: minor ?? Number.NaN };
}

/** Более новая из двух версий одного major (в таблицах только major 1). */
const newerOf = (left: ProtocolVersion, right: ProtocolVersion): ProtocolVersion => (right.minor > left.minor ? right : left);

const types = readTable<SideTable>("message-versions.json");
const fields = readTable<SideTable>("field-versions.json");
const values = readTable<Readonly<Record<string, Readonly<Record<string, string>>>>>("value-versions.json");

/**
 * Версия, которой по общим таблицам требует сообщение фикстуры: самая новая из версии типа (message-versions.json),
 * версий присутствующих необязательных полей (field-versions.json, с 1.10) и версий значений (value-versions.json,
 * с 1.3). Независимая от кода оценка: только JSON фикстуры и таблицы.
 */
function tableRequiredVersion(message: Readonly<Record<string, unknown>>, side: "page_session" | "player_session"): ProtocolVersion {
  const type = String(message["type"]);
  let required = versionOf(types[side][type] ?? "");
  for (const [field, value] of Object.entries(message)) {
    const key = `${type}.${field}`;
    const fieldVersion = fields[side][key];
    if (fieldVersion !== undefined) required = newerOf(required, versionOf(fieldVersion));
    const valueVersion = typeof value === "string" ? values[key]?.[value] : undefined;
    if (valueVersion !== undefined) required = newerOf(required, versionOf(valueVersion));
  }
  return required;
}

test("каждая валидная фикстура сеанса требует ровно ту версию, что дают общие таблицы (тик с номером - 1.10)", () => {
  let ticksWithGeneration = 0;
  for (const fixture of manifest.cases) {
    if (fixture.expect.result !== "ok" || (fixture.schema !== "page_session" && fixture.schema !== "player_session")) continue;
    const bytes = readFixtureBytes(fixture.file);
    const json = JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>;
    const expected = tableRequiredVersion(json, fixture.schema);
    if (fixture.schema === "page_session") {
      const decoded = decodePageSession(bytes);
      assert.ok(decoded.ok, fixture.file);
      assert.deepEqual(pageSessionRequiredVersion(decoded.value), expected, fixture.file);
    } else {
      const decoded = decodePlayerSession(bytes);
      assert.ok(decoded.ok, fixture.file);
      assert.deepEqual(playerSessionRequiredVersion(decoded.value), expected, fixture.file);
      if (json["type"] === "clock" && "generation" in json) {
        assert.deepEqual(expected, { major: 1, minor: 10 }, fixture.file);
        ticksWithGeneration += 1;
      }
    }
  }
  // Проверка не пустая: тики с номером поколения среди фикстур есть.
  assert.ok(ticksWithGeneration >= 3, `тиков с номером: ${ticksWithGeneration}`);
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
