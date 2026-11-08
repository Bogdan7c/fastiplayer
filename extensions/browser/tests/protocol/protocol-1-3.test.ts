// Договоры протокола 1.3 (сессия 11a, решения 236-237), которых не покрывает общий проход
// по manifest.json (его делает fixtures.test.ts для тех же фикстур `tab_gone`):
// - в 1.3 нет новых сообщений (fixtures/message-versions.json), новое - одно ЗНАЧЕНИЕ
//   старого: `tab_gone{reason: "page_player_failed"}`;
// - версия каждого значения - общая таблица fixtures/value-versions.json, полная по
//   значениям (tabGoneReasonIntroducedIn);
// - собеседник с общей версией 1.2 и ниже нового значения не получает;
// - новое значение пишется ровно задокументированным JSON и читается обратно, метка строгая;
// - код закрытия 4010 `viewer_returned_to_page` совпадает с Rust и не версионируется.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_3.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_1,
  PROTOCOL_VERSION_1_2,
  PROTOCOL_VERSION_1_3,
  TAB_GONE_REASONS,
  closeCodeOf,
  decodePageSession,
  encodePageSession,
  negotiateProtocolVersion,
  pageSessionIntroducedIn,
  tabGoneReasonIntroducedIn,
  versionIncludes,
  type PageSessionMessage,
  type ProtocolVersion,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

function readJson<T>(name: string): T {
  return JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as T;
}

/** Версия из записи фикстуры `"1.3"`. */
function versionOf(text: string): ProtocolVersion {
  const [major, minor] = text.split(".").map(Number);
  return { major: major ?? Number.NaN, minor: minor ?? Number.NaN };
}

/** Старые причины ухода вкладки - есть в любой версии 1.x. */
const OLDER_REASONS = ["closed", "navigated_away", "browser_closing"] as const;

test("в 1.3 нет новых типов сообщений: tab_gone по-прежнему из 1.0", () => {
  const table = readJson<Record<string, Record<string, string>>>("message-versions.json");
  const newer = Object.values(table)
    .flatMap((byType) => Object.entries(byType))
    .filter(([, version]) => version === "1.3")
    .map(([type]) => type);
  assert.deepEqual(newer, []);
  assert.deepEqual(pageSessionIntroducedIn("tab_gone"), PROTOCOL_VERSION_1_0);
});

test("версия каждой причины tab_gone - ровно та, что в общей таблице значений", () => {
  const table = readJson<{ "tab_gone.reason": Record<string, string> }>("value-versions.json");
  assert.deepEqual(Object.keys(table).sort(), ["tab_gone.reason"]);
  const byReason = table["tab_gone.reason"];
  // Таблица полна по значениям: тот же список, что разбирает кодек.
  assert.deepEqual(Object.keys(byReason).sort(), [...TAB_GONE_REASONS].sort());
  for (const reason of TAB_GONE_REASONS) {
    assert.deepEqual(tabGoneReasonIntroducedIn(reason), versionOf(byReason[reason] ?? ""), reason);
  }
  const newer = Object.entries(byReason)
    .filter(([, version]) => version === "1.3")
    .map(([reason]) => reason);
  assert.deepEqual(newer, ["page_player_failed"]);
});

test("page_player_failed доходит только до собеседника, договорившегося на 1.3", () => {
  const introducedIn = tabGoneReasonIntroducedIn("page_player_failed");
  assert.deepEqual(introducedIn, PROTOCOL_VERSION_1_3);
  // Новый экстеншен со старым плеером: общая версия - старая, значения в ней нет.
  for (const olderPlayer of [PROTOCOL_VERSION_1_2, PROTOCOL_VERSION_1_1, PROTOCOL_VERSION_1_0]) {
    const agreed = negotiateProtocolVersion(olderPlayer, CURRENT_PROTOCOL_VERSION);
    assert.deepEqual(agreed, { ok: true, version: olderPlayer });
    assert.equal(agreed.ok && versionIncludes(agreed.version, introducedIn), false);
    // Старые причины - в любой общей версии.
    for (const reason of OLDER_REASONS) {
      assert.equal(agreed.ok && versionIncludes(agreed.version, tabGoneReasonIntroducedIn(reason)), true, reason);
    }
  }
  const agreed = negotiateProtocolVersion(PROTOCOL_VERSION_1_3, CURRENT_PROTOCOL_VERSION);
  assert.equal(agreed.ok && versionIncludes(agreed.version, introducedIn), true);
});

test("page_player_failed пишется ровно задокументированным JSON и читается обратно", () => {
  const message: PageSessionMessage = { type: "tab_gone", reason: "page_player_failed" };
  const json = '{"type":"tab_gone","reason":"page_player_failed"}';
  const written = encodePageSession(message);
  assert.ok(written.ok);
  assert.equal(written.value, json);
  assert.deepEqual(decodePageSession(json), { ok: true, value: message });
  // Тот же текст лежит в общей фикстуре.
  const fixture = readFileSync(join(FIXTURES_ROOT, "session/valid/page-tab-gone-page-player-failed.json"), "utf8");
  assert.equal(fixture.trimEnd(), json);
});

test("метка page_player_failed строгая: регистр, дефисы и пробелы - unknown_value, как в Rust", () => {
  for (const nearMiss of ["Page_Player_Failed", "page-player-failed", "page_player_failed "]) {
    const decoded = decodePageSession(`{"type":"tab_gone","reason":${JSON.stringify(nearMiss)}}`);
    assert.equal(decoded.ok ? "ok" : decoded.error.kind, "unknown_value", nearMiss);
  }
});

test("4010 viewer_returned_to_page - тот же код, что в Rust", () => {
  assert.equal(closeCodeOf("viewer_returned_to_page"), 4010);
  // Код читается прямо из исходника Rust: расхождение ловится без отдельной фикстуры.
  const rustSource = readFileSync(join(FIXTURES_ROOT, "..", "src", "session", "mod.rs"), "utf8");
  assert.match(rustSource, /Self::ViewerReturnedToPage => 4010,/);
});
