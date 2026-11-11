// Договор протокола 1.8 (сессия 15c2, решение 301 - исход выбора на странице), которого не покрывает общий
// проход по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - ровно одно новое сообщение: `variant_outcome` (страница -> плеер) - fixtures/message-versions.json;
// - пишется ровно задокументированным JSON и читается обратно; лишнее поле, чужой вид запроса, незнакомый
//   исход, пустой `variant_id` - отказ;
// - доходит только до собеседника, договорившегося на 1.8.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_8.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  PROTOCOL_VERSION_1_7,
  PROTOCOL_VERSION_1_8,
  VARIANT_OUTCOMES,
  decodePageSession,
  encodePageSession,
  negotiateProtocolVersion,
  pageSessionIntroducedIn,
  versionIncludes,
  type PageSessionMessage,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

const table = JSON.parse(readFileSync(join(FIXTURES_ROOT, "message-versions.json"), "utf8")) as Record<string, Record<string, string>>;
const newerIn18 = (side: string): string[] =>
  Object.entries(table[side] ?? {})
    .filter(([, version]) => version === "1.8")
    .map(([type]) => type);

test("в 1.8 ровно одно новое сообщение страницы; экстеншен реализует именно 1.8", () => {
  assert.deepEqual(newerIn18("page_session"), ["variant_outcome"]);
  assert.deepEqual(newerIn18("player_session"), []);
  assert.deepEqual(pageSessionIntroducedIn("variant_outcome"), PROTOCOL_VERSION_1_8);
  assert.deepEqual(CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_8);
});

test("variant_outcome пишется ровно задокументированным JSON и читается обратно: выбор и Next, все исходы", () => {
  const cases: { json: string; value: PageSessionMessage }[] = [
    {
      json: '{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":"dub-b"},"outcome":"no_such_episode"}',
      value: { type: "variant_outcome", request: { kind: "select_variant", menu: "audio", variant_id: "dub-b" }, outcome: "no_such_episode" },
    },
    {
      json: '{"type":"variant_outcome","request":{"kind":"next_episode"},"outcome":"done"}',
      value: { type: "variant_outcome", request: { kind: "next_episode" }, outcome: "done" },
    },
  ];
  for (const outcome of VARIANT_OUTCOMES) {
    cases.push({
      json: `{"type":"variant_outcome","request":{"kind":"select_variant","menu":"quality","variant_id":"video-720"},"outcome":"${outcome}"}`,
      value: { type: "variant_outcome", request: { kind: "select_variant", menu: "quality", variant_id: "video-720" }, outcome },
    });
  }
  assert.deepEqual([...VARIANT_OUTCOMES], ["done", "no_such_episode", "person_only", "failed"]);
  for (const { json, value } of cases) {
    assert.deepEqual(encodePageSession(value), { ok: true, value: json });
    assert.deepEqual(decodePageSession(json), { ok: true, value });
  }
});

test("лишнее поле, чужой вид запроса, незнакомый исход, пустой variant_id - отказ", () => {
  const selectAudio = '{"kind":"select_variant","menu":"audio","variant_id":"dub-b"}';
  const rejected = [
    `{"type":"variant_outcome","request":${selectAudio},"outcome":"done","note":"x"}`,
    '{"type":"variant_outcome","request":{"kind":"next_episode","menu":"audio"},"outcome":"done"}',
    '{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":"dub-b","extra":1},"outcome":"done"}',
    '{"type":"variant_outcome","request":{"kind":"skip_ad"},"outcome":"done"}',
    '{"type":"variant_outcome","request":{"kind":"select_variant","menu":"video","variant_id":"dub-b"},"outcome":"done"}',
    '{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio"},"outcome":"done"}',
    '{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":""},"outcome":"done"}',
    `{"type":"variant_outcome","request":${selectAudio},"outcome":"no_such_variant"}`,
    `{"type":"variant_outcome","request":${selectAudio},"outcome":true}`,
    `{"type":"variant_outcome","request":${selectAudio}}`,
    '{"type":"variant_outcome","outcome":"done"}',
    '{"type":"variant_outcome","request":"next_episode","outcome":"done"}',
  ];
  for (const json of rejected) assert.equal(decodePageSession(json).ok, false, json);
  // Пустой variant_id не пишется и в сторону плеера.
  assert.equal(
    encodePageSession({ type: "variant_outcome", request: { kind: "select_variant", menu: "audio", variant_id: "" }, outcome: "done" }).ok,
    false,
  );
});

test("variant_outcome доходит только до собеседника, договорившегося на 1.8", () => {
  const older = negotiateProtocolVersion(PROTOCOL_VERSION_1_7, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(older, { ok: true, version: PROTOCOL_VERSION_1_7 });
  assert.equal(older.ok && versionIncludes(older.version, pageSessionIntroducedIn("variant_outcome")), false);
  const both = negotiateProtocolVersion(PROTOCOL_VERSION_1_8, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(both, { ok: true, version: PROTOCOL_VERSION_1_8 });
  assert.equal(both.ok && versionIncludes(both.version, pageSessionIntroducedIn("variant_outcome")), true);
  // Сообщения 1.7 остаются доступны на 1.8.
  assert.equal(both.ok && versionIncludes(both.version, pageSessionIntroducedIn("page_navigation")), true);
});
