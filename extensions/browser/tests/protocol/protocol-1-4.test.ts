// Договоры протокола 1.4 (сессия 11b1, решения 240-241 - «возврат вкладки»), которых не
// покрывает общий проход по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - четыре новых сообщения (fixtures/message-versions.json): `page_left`, `page_returned`,
//   `page_reopen_failed` и `reopen_page`;
// - версии значений `page_reopen_failed.reason` - общая таблица fixtures/value-versions.json,
//   полная по значениям;
// - при общей версии 1.3 и ниже новых сообщений нет;
// - каждое сообщение пишется ровно задокументированным JSON и читается обратно, метки строгие.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_4.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  PAGE_REOPEN_FAILURES,
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_3,
  PROTOCOL_VERSION_1_4,
  TAB_GONE_REASONS,
  decodePageSession,
  decodePlayerSession,
  encodePageSession,
  encodePlayerSession,
  negotiateProtocolVersion,
  pageReopenFailureIntroducedIn,
  pageSessionIntroducedIn,
  pageSessionRequiredVersion,
  playerSessionIntroducedIn,
  versionIncludes,
  type PageSessionMessage,
  type PlayerSessionMessage,
  type ProtocolVersion,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

function readJson<T>(name: string): T {
  return JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as T;
}

/** Версия из записи фикстуры `"1.4"`. */
function versionOf(text: string): ProtocolVersion {
  const [major, minor] = text.split(".").map(Number);
  return { major: major ?? Number.NaN, minor: minor ?? Number.NaN };
}

test("в 1.4 ровно четыре новых типа сообщений, tab_gone по-прежнему из 1.0", () => {
  const table = readJson<Record<string, Record<string, string>>>("message-versions.json");
  const newer = (side: string): string[] =>
    Object.entries(table[side] ?? {})
      .filter(([, version]) => version === "1.4")
      .map(([type]) => type)
      .sort();
  assert.deepEqual(newer("page_session"), ["page_left", "page_reopen_failed", "page_returned"]);
  assert.deepEqual(newer("player_session"), ["reopen_page"]);
  assert.deepEqual(pageSessionIntroducedIn("tab_gone"), PROTOCOL_VERSION_1_0);
  for (const type of ["page_left", "page_returned", "page_reopen_failed"] as const) {
    assert.deepEqual(pageSessionIntroducedIn(type), versionOf(table["page_session"]?.[type] ?? ""), type);
    assert.deepEqual(pageSessionIntroducedIn(type), PROTOCOL_VERSION_1_4, type);
  }
  assert.deepEqual(playerSessionIntroducedIn("reopen_page"), PROTOCOL_VERSION_1_4);
});

test("версия каждого отказа возврата - ровно та, что в общей таблице значений", () => {
  const table = readJson<Record<string, Record<string, string>>>("value-versions.json");
  const byFailure = table["page_reopen_failed.reason"] ?? {};
  assert.deepEqual(Object.keys(byFailure).sort(), [...PAGE_REOPEN_FAILURES].sort(), "таблица полна по значениям");
  for (const failure of PAGE_REOPEN_FAILURES) {
    assert.deepEqual(pageReopenFailureIntroducedIn(failure), versionOf(byFailure[failure] ?? ""), failure);
    assert.deepEqual(pageReopenFailureIntroducedIn(failure), PROTOCOL_VERSION_1_4, failure);
  }
});

test("требуемая версия новых сообщений страницы - 1.4 при любой причине, старых - прежняя", () => {
  for (const reason of TAB_GONE_REASONS) {
    assert.deepEqual(pageSessionRequiredVersion({ type: "page_left", reason }), PROTOCOL_VERSION_1_4, reason);
  }
  assert.deepEqual(pageSessionRequiredVersion({ type: "page_returned" }), PROTOCOL_VERSION_1_4);
  for (const failure of PAGE_REOPEN_FAILURES) {
    assert.deepEqual(pageSessionRequiredVersion({ type: "page_reopen_failed", reason: failure }), PROTOCOL_VERSION_1_4, failure);
  }
  // Без изменений: тип tab_gone старый, значение page_player_failed - 1.3.
  assert.deepEqual(pageSessionRequiredVersion({ type: "tab_gone", reason: "closed" }), PROTOCOL_VERSION_1_0);
  assert.deepEqual(pageSessionRequiredVersion({ type: "tab_gone", reason: "page_player_failed" }), PROTOCOL_VERSION_1_3);
});

test("новые сообщения доходят только до собеседника, договорившегося на 1.4", () => {
  const agreed = negotiateProtocolVersion(PROTOCOL_VERSION_1_3, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(agreed, { ok: true, version: PROTOCOL_VERSION_1_3 });
  assert.equal(agreed.ok && versionIncludes(agreed.version, PROTOCOL_VERSION_1_4), false);
  const both = negotiateProtocolVersion(CURRENT_PROTOCOL_VERSION, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(both, { ok: true, version: PROTOCOL_VERSION_1_4 });
  assert.equal(both.ok && versionIncludes(both.version, PROTOCOL_VERSION_1_4), true);
});

test("новые сообщения пишутся ровно задокументированным JSON и читаются обратно", () => {
  const pageCases: ReadonlyArray<readonly [PageSessionMessage, string]> = [
    [{ type: "page_left", reason: "closed" }, '{"type":"page_left","reason":"closed"}'],
    [{ type: "page_left", reason: "page_player_failed" }, '{"type":"page_left","reason":"page_player_failed"}'],
    [{ type: "page_returned" }, '{"type":"page_returned"}'],
    [{ type: "page_reopen_failed", reason: "no_browser_window" }, '{"type":"page_reopen_failed","reason":"no_browser_window"}'],
    [{ type: "page_reopen_failed", reason: "video_did_not_start" }, '{"type":"page_reopen_failed","reason":"video_did_not_start"}'],
  ];
  for (const [message, json] of pageCases) {
    const written = encodePageSession(message);
    assert.ok(written.ok);
    assert.equal(written.value, json);
    assert.deepEqual(decodePageSession(json), { ok: true, value: message });
  }
  const player: PlayerSessionMessage = { type: "reopen_page", position_us: 123 };
  const written = encodePlayerSession(player);
  assert.ok(written.ok);
  assert.equal(written.value, '{"type":"reopen_page","position_us":123}');
  assert.deepEqual(decodePlayerSession(written.value), { ok: true, value: player });
});

test("метки отказа строгие, page_returned без полей, reopen_page требует позицию", () => {
  for (const nearMiss of ["No_Browser_Window", "no-browser-window", " no_browser_window", "teleported", "closed"]) {
    const decoded = decodePageSession(`{"type":"page_reopen_failed","reason":${JSON.stringify(nearMiss)}}`);
    assert.equal(decoded.ok ? "ok" : decoded.error.kind, "unknown_value", nearMiss);
  }
  const extra = decodePageSession('{"type":"page_returned","reason":"closed"}');
  assert.equal(extra.ok ? "ok" : extra.error.kind, "unknown_field");
  const missing = decodePlayerSession('{"type":"reopen_page"}');
  assert.equal(missing.ok ? "ok" : missing.error.kind, "missing_field");
  const negative = decodePlayerSession('{"type":"reopen_page","position_us":-1}');
  assert.equal(negative.ok ? "ok" : negative.error.kind, "number_out_of_range");
});
