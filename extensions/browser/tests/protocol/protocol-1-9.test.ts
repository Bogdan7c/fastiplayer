// Договор протокола 1.9 (сессия 16a - смена серии на той же странице), которого не покрывает общий проход по
// manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - ровно два новых сообщения страницы: `awaiting_next_episode` и `page_title_changed` -
//   fixtures/message-versions.json;
// - пишутся ровно задокументированным JSON и читаются обратно; лишнее поле, чужой тип, срок вне u32 - отказ;
// - заголовок проверяется тем же правилом, что `title` в `open_session`, а `pageTitleChangedMessage` чистит его
//   так же, как заголовок при открытии (`sanitizePageTitle`);
// - доходят только до собеседника, договорившегося на 1.9; код 4011 - тоже, иначе прежний 4009.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_9.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  MAX_PAGE_TITLE_BYTES,
  NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN,
  PROTOCOL_VERSION_1_8,
  PROTOCOL_VERSION_1_9,
  closeCodeOf,
  closeReasonForUnstartedNextEpisode,
  closeReasonFromCode,
  decodeExtensionControl,
  decodePageSession,
  encodePageSession,
  negotiateProtocolVersion,
  pageSessionIntroducedIn,
  pageSessionRequiredVersion,
  pageTitleChangedMessage,
  sanitizePageTitle,
  utf8ByteLength,
  versionIncludes,
  type PageSessionMessage,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

const table = JSON.parse(readFileSync(join(FIXTURES_ROOT, "message-versions.json"), "utf8")) as Record<string, Record<string, string>>;
const newerIn19 = (side: string): string[] =>
  Object.entries(table[side] ?? {})
    .filter(([, version]) => version === "1.9")
    .map(([type]) => type);

/** Сообщения 1.9 и их точный JSON на проводе. */
const DOCUMENTED: readonly { json: string; value: PageSessionMessage }[] = [
  { json: '{"type":"awaiting_next_episode","patience_ms":60000}', value: { type: "awaiting_next_episode", patience_ms: 60_000 } },
  { json: '{"type":"awaiting_next_episode","patience_ms":4294967295}', value: { type: "awaiting_next_episode", patience_ms: 0xffff_ffff } },
  { json: '{"type":"page_title_changed","title":"Серія 4 — Озвучка 🎬"}', value: { type: "page_title_changed", title: "Серія 4 — Озвучка 🎬" } },
  // Страница без `<title>`: пустой заголовок допустим, как в `open_session`.
  { json: '{"type":"page_title_changed","title":""}', value: { type: "page_title_changed", title: "" } },
];

test("в 1.9 ровно два новых сообщения страницы; экстеншен реализует 1.9 или новее", () => {
  assert.deepEqual(newerIn19("page_session").sort(), ["awaiting_next_episode", "page_title_changed"]);
  assert.deepEqual(newerIn19("player_session"), []);
  for (const { value } of DOCUMENTED) {
    assert.deepEqual(pageSessionIntroducedIn(value.type), PROTOCOL_VERSION_1_9);
    assert.deepEqual(pageSessionRequiredVersion(value), PROTOCOL_VERSION_1_9);
  }
  // С 1.10 экстеншен говорит на более новой версии, а 1.9 в неё входит.
  assert.equal(versionIncludes(CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_9), true);
});

test("awaiting_next_episode и page_title_changed пишутся ровно задокументированным JSON и читаются обратно", () => {
  for (const { json, value } of DOCUMENTED) {
    assert.deepEqual(encodePageSession(value), { ok: true, value: json });
    assert.deepEqual(decodePageSession(json), { ok: true, value });
  }
});

test("лишнее поле, чужой тип и срок вне u32 - отказ, и в сторону плеера такое не пишется", () => {
  const rejected = [
    '{"type":"awaiting_next_episode","patience_ms":60000,"title":"x"}',
    '{"type":"awaiting_next_episode"}',
    '{"type":"awaiting_next_episode","patience_ms":"60000"}',
    '{"type":"awaiting_next_episode","patience_ms":null}',
    '{"type":"awaiting_next_episode","patience_ms":1.5}',
    '{"type":"awaiting_next_episode","patience_ms":-1}',
    '{"type":"awaiting_next_episode","patience_ms":4294967296}',
    '{"type":"page_title_changed","title":"x","domain":"example.com"}',
    '{"type":"page_title_changed"}',
    '{"type":"page_title_changed","title":4}',
    '{"type":"page_title_changed","title":null}',
  ];
  for (const json of rejected) assert.equal(decodePageSession(json).ok, false, json);
  assert.equal(encodePageSession({ type: "awaiting_next_episode", patience_ms: -1 }).ok, false);
  assert.equal(encodePageSession({ type: "awaiting_next_episode", patience_ms: 0x1_0000_0000 }).ok, false);
  assert.equal(encodePageSession({ type: "page_title_changed", title: "a\u0000b" }).ok, false);
});

test("новый заголовок проверяется ровно правилом title из open_session", () => {
  // Одно правило на оба места: для каждого заголовка итог `page_title_changed` совпадает с итогом
  // `open_session` с тем же заголовком (и при приёме, и при отказе, с той же причиной).
  const atLimit = "ї".repeat(MAX_PAGE_TITLE_BYTES / 2);
  const titles = ["Серія 4", "", atLimit, `${atLimit}x`, "movie‮4pm.exe", "a\u0000b", "Серія 4\nОзвучка"];
  for (const title of titles) {
    const changed = decodePageSession(JSON.stringify({ type: "page_title_changed", title }));
    const opened = decodeExtensionControl(
      // Та же форма, что у фикстуры control/valid/open-session-in-page-untitled.json, но с этим заголовком.
      JSON.stringify({
        type: "open_session",
        request_id: 3,
        session_key: "tab-12-video-3",
        page: { title, domain: "example.com" },
        intent: "in_page",
        hints: { duration: { kind: "unknown" }, position: { kind: "unknown" } },
      }),
    );
    assert.equal(changed.ok, opened.ok, JSON.stringify(title));
    // Хотя бы допустимые заголовки должны открываться: иначе сравнение ничего не проверяет.
    if (title === "Серія 4" || title === "" || title === atLimit) assert.equal(opened.ok, true, JSON.stringify(title));
    if (!changed.ok && !opened.ok) assert.equal(changed.error.kind, opened.error.kind, JSON.stringify(title));
  }
});

test("pageTitleChangedMessage чистит заголовок как при открытии, и результат всегда проходит протокол", () => {
  const raw = "Серія 4\nОзвучка‮ " + "ї".repeat(MAX_PAGE_TITLE_BYTES);
  const message = pageTitleChangedMessage(raw);
  assert.equal(message.type, "page_title_changed");
  assert.equal(message.title, sanitizePageTitle(raw));
  assert.ok(message.title.startsWith("Серія 4 Озвучка "));
  assert.ok(utf8ByteLength(message.title) <= MAX_PAGE_TITLE_BYTES);
  assert.equal(encodePageSession(message).ok, true);
  assert.deepEqual(pageTitleChangedMessage(""), { type: "page_title_changed", title: "" });
});

test("слова 1.9 доходят только до собеседника, договорившегося на 1.9", () => {
  const older = negotiateProtocolVersion(PROTOCOL_VERSION_1_8, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(older, { ok: true, version: PROTOCOL_VERSION_1_8 });
  const both = negotiateProtocolVersion(PROTOCOL_VERSION_1_9, CURRENT_PROTOCOL_VERSION);
  assert.deepEqual(both, { ok: true, version: PROTOCOL_VERSION_1_9 });
  for (const type of ["awaiting_next_episode", "page_title_changed"] as const) {
    assert.equal(older.ok && versionIncludes(older.version, pageSessionIntroducedIn(type)), false, type);
    assert.equal(both.ok && versionIncludes(both.version, pageSessionIntroducedIn(type)), true, type);
  }
  // Сообщения 1.8 остаются доступны на 1.9.
  assert.equal(both.ok && versionIncludes(both.version, pageSessionIntroducedIn("variant_outcome")), true);
});

test("код 4011 next_episode_not_started в обе стороны; со старым плеером - прежний 4009", () => {
  assert.equal(closeCodeOf("next_episode_not_started"), 4011);
  assert.equal(closeReasonFromCode(4011), "next_episode_not_started");
  assert.equal(closeReasonFromCode(4012), undefined);
  assert.equal(closeCodeOf("page_took_video_back"), 4009);
  assert.deepEqual(NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN, PROTOCOL_VERSION_1_9);
  assert.equal(closeReasonForUnstartedNextEpisode(PROTOCOL_VERSION_1_9), "next_episode_not_started");
  assert.equal(closeReasonForUnstartedNextEpisode(PROTOCOL_VERSION_1_8), "page_took_video_back");
  // Будущая minor того же major по-прежнему знает 4011.
  assert.equal(closeReasonForUnstartedNextEpisode({ major: 1, minor: 10 }), "next_episode_not_started");
});
