// Договоры протокола 1.6 (сессия 13c, решение 278 - «показать страницу»), которых не покрывает общий проход
// по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - одно новое сообщение плеера `show_page` (fixtures/message-versions.json), у страницы новых нет;
// - оно пишется ровно задокументированным JSON, читается обратно и не принимает полей;
// - при общей версии 1.5 и ниже оно в версию не входит (плеер его не шлёт).
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_6.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  PROTOCOL_VERSION_1_5,
  PROTOCOL_VERSION_1_6,
  decodePlayerSession,
  encodePlayerSession,
  negotiateProtocolVersion,
  playerSessionIntroducedIn,
  versionIncludes,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

const table = JSON.parse(readFileSync(join(FIXTURES_ROOT, "message-versions.json"), "utf8")) as Record<string, Record<string, string>>;
const newerIn16 = (side: string): string[] =>
  Object.entries(table[side] ?? {})
    .filter(([, version]) => version === "1.6")
    .map(([type]) => type);

test("в 1.6 ровно одно новое сообщение - show_page от плеера; экстеншен реализует 1.6 и новее", () => {
  assert.deepEqual(newerIn16("player_session"), ["show_page"]);
  assert.deepEqual(newerIn16("page_session"), []);
  assert.deepEqual(playerSessionIntroducedIn("show_page"), PROTOCOL_VERSION_1_6);
  assert.equal(versionIncludes(CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_6), true);
});

test("show_page пишется ровно задокументированным JSON, читается обратно и не берёт лишних полей", () => {
  assert.deepEqual(encodePlayerSession({ type: "show_page" }), { ok: true, value: '{"type":"show_page"}' });
  assert.deepEqual(decodePlayerSession('{"type":"show_page"}'), { ok: true, value: { type: "show_page" } });
  const withField = decodePlayerSession('{"type":"show_page","tab":1}');
  assert.equal(withField.ok, false, "лишнее поле - отказ");
});

test("show_page доходит только до собеседника, договорившегося на 1.6", () => {
  const older = negotiateProtocolVersion(PROTOCOL_VERSION_1_5, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(older, { ok: true, version: PROTOCOL_VERSION_1_5 });
  assert.equal(older.ok && versionIncludes(older.version, playerSessionIntroducedIn("show_page")), false);
  const both = negotiateProtocolVersion(PROTOCOL_VERSION_1_6, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(both, { ok: true, version: PROTOCOL_VERSION_1_6 });
  assert.equal(both.ok && versionIncludes(both.version, playerSessionIntroducedIn("show_page")), true);
});
