// Договоры протокола 1.7 (сессия 14a - варианты страницы в окне плеера), которых не покрывает общий проход
// по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - три новых сообщения: `page_navigation`, `subtitle_preference` (страница -> плеер), `subtitle_choice`
//   (плеер -> страница) - fixtures/message-versions.json;
// - каждое пишется ровно задокументированным JSON и читается обратно; лишние поля, номер дорожки 0 и чужой
//   вид выбора - отказ;
// - при общей версии 1.6 и ниже они в версию не входят.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_7.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  PROTOCOL_VERSION_1_6,
  PROTOCOL_VERSION_1_7,
  decodePageSession,
  decodePlayerSession,
  encodePageSession,
  encodePlayerSession,
  negotiateProtocolVersion,
  pageSessionIntroducedIn,
  playerSessionIntroducedIn,
  versionIncludes,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

const table = JSON.parse(readFileSync(join(FIXTURES_ROOT, "message-versions.json"), "utf8")) as Record<string, Record<string, string>>;
const newerIn17 = (side: string): string[] =>
  Object.entries(table[side] ?? {})
    .filter(([, version]) => version === "1.7")
    .map(([type]) => type);

test("в 1.7 ровно три новых сообщения; экстеншен реализует именно 1.7", () => {
  assert.deepEqual(newerIn17("page_session"), ["page_navigation", "subtitle_preference"]);
  assert.deepEqual(newerIn17("player_session"), ["subtitle_choice"]);
  assert.deepEqual(pageSessionIntroducedIn("page_navigation"), PROTOCOL_VERSION_1_7);
  assert.deepEqual(pageSessionIntroducedIn("subtitle_preference"), PROTOCOL_VERSION_1_7);
  assert.deepEqual(playerSessionIntroducedIn("subtitle_choice"), PROTOCOL_VERSION_1_7);
  assert.deepEqual(CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_7);
  // Сообщения 1.0 про варианты не менялись.
  assert.deepEqual(pageSessionIntroducedIn("variant_menu_changed"), { major: 1, minor: 0 });
  assert.deepEqual(playerSessionIntroducedIn("select_variant"), { major: 1, minor: 0 });
  assert.deepEqual(playerSessionIntroducedIn("next_episode"), { major: 1, minor: 0 });
});

test("сообщения страницы пишутся ровно задокументированным JSON и читаются обратно", () => {
  const cases = [
    { json: '{"type":"page_navigation","next_episode":"available"}', value: { type: "page_navigation", next_episode: "available" } },
    { json: '{"type":"page_navigation","next_episode":"unavailable"}', value: { type: "page_navigation", next_episode: "unavailable" } },
    {
      json: '{"type":"subtitle_preference","choice":{"kind":"page_track","track":3}}',
      value: { type: "subtitle_preference", choice: { kind: "page_track", track: 3 } },
    },
    { json: '{"type":"subtitle_preference","choice":{"kind":"off"}}', value: { type: "subtitle_preference", choice: { kind: "off" } } },
  ] as const;
  for (const { json, value } of cases) {
    assert.deepEqual(encodePageSession(value), { ok: true, value: json });
    assert.deepEqual(decodePageSession(json), { ok: true, value });
  }
});

test("subtitle_choice пишется ровно задокументированным JSON и читается обратно", () => {
  const cases = [
    {
      json: '{"type":"subtitle_choice","choice":{"kind":"page_track","track":7}}',
      value: { type: "subtitle_choice", choice: { kind: "page_track", track: 7 } },
    },
    { json: '{"type":"subtitle_choice","choice":{"kind":"off"}}', value: { type: "subtitle_choice", choice: { kind: "off" } } },
  ] as const;
  for (const { json, value } of cases) {
    assert.deepEqual(encodePlayerSession(value), { ok: true, value: json });
    assert.deepEqual(decodePlayerSession(json), { ok: true, value });
  }
});

test("лишние поля, номер дорожки 0, чужой вид и значение - отказ", () => {
  const page = [
    '{"type":"page_navigation","next_episode":"available","url":"x"}',
    '{"type":"page_navigation","next_episode":"maybe"}',
    '{"type":"page_navigation"}',
    '{"type":"subtitle_preference","choice":{"kind":"off","track":1}}',
    '{"type":"subtitle_preference","choice":{"kind":"page_track","track":0}}',
    '{"type":"subtitle_preference","choice":{"kind":"own_file"}}',
    '{"type":"subtitle_preference","choice":"off"}',
  ];
  for (const json of page) assert.equal(decodePageSession(json).ok, false, json);
  const player = [
    '{"type":"subtitle_choice","choice":{"kind":"off"},"auto":true}',
    '{"type":"subtitle_choice","choice":{"kind":"page_track","track":0}}',
    '{"type":"subtitle_choice","choice":{"kind":"embedded","track":1}}',
  ];
  for (const json of player) assert.equal(decodePlayerSession(json).ok, false, json);
});

test("новые сообщения доходят только до собеседника, договорившегося на 1.7", () => {
  const older = negotiateProtocolVersion(PROTOCOL_VERSION_1_6, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(older, { ok: true, version: PROTOCOL_VERSION_1_6 });
  for (const type of ["page_navigation", "subtitle_preference"] as const) {
    assert.equal(older.ok && versionIncludes(older.version, pageSessionIntroducedIn(type)), false, type);
  }
  assert.equal(older.ok && versionIncludes(older.version, playerSessionIntroducedIn("subtitle_choice")), false);
  const both = negotiateProtocolVersion(PROTOCOL_VERSION_1_7, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(both, { ok: true, version: PROTOCOL_VERSION_1_7 });
  assert.equal(both.ok && versionIncludes(both.version, playerSessionIntroducedIn("subtitle_choice")), true);
});
