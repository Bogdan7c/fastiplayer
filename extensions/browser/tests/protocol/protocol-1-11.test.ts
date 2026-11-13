// Договор протокола 1.11 (мост 16d1, решения 325-327 - окно записи `recording_window`), которого не покрывает
// общий проход по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - новое сообщение ровно одно (fixtures/message-versions.json), полей у старых сообщений 1.11 не меняет;
// - оно пишется ровно задокументированным JSON и читается обратно тем же значением;
// - инвариант `start <= end(окна) <= end_us` держится при разборе и при записи, с точной причиной;
// - строгость формы (пропавшее/лишнее поле, `null`, метка) не ослаблена;
// - оно требует общей версии 1.11: с пиром 1.10 общая версия 1.10 и окно не уходит.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_11.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  PROTOCOL_VERSION_1_10,
  PROTOCOL_VERSION_1_11,
  decodePlayerSession,
  encodePlayerSession,
  negotiateProtocolVersion,
  playerSessionIntroducedIn,
  playerSessionRequiredVersion,
  versionIncludes,
  type LiveSeekable,
  type PlayerSessionMessage,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

const readTable = (name: string): Record<string, Record<string, string>> =>
  JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as Record<string, Record<string, string>>;

function recordingWindow(generation: number, endUs: number, seekable: LiveSeekable): PlayerSessionMessage {
  return { type: "recording_window", generation, end_us: endUs, seekable };
}

/** Сообщения и их точный JSON на проводе. */
const DOCUMENTED: readonly { json: string; value: PlayerSessionMessage }[] = [
  {
    json: '{"type":"recording_window","generation":3,"end_us":61000000,"seekable":{"kind":"range","start_us":0,"end_us":61000000}}',
    value: recordingWindow(3, 61_000_000, { kind: "range", start_us: 0, end_us: 61_000_000 }),
  },
  {
    json: '{"type":"recording_window","generation":1,"end_us":5,"seekable":{"kind":"none"}}',
    value: recordingWindow(1, 5, { kind: "none" }),
  },
  // Окно в одну точку ровно на конце записи - край договора включительно.
  {
    json: '{"type":"recording_window","generation":2,"end_us":9,"seekable":{"kind":"range","start_us":9,"end_us":9}}',
    value: recordingWindow(2, 9, { kind: "range", start_us: 9, end_us: 9 }),
  },
];

test("в 1.11 нового ровно одно сообщение - recording_window; экстеншен реализует 1.11", () => {
  const messages = readTable("message-versions.json");
  const newIn111 = Object.values(messages).flatMap((side) => Object.entries(side).filter(([, version]) => version === "1.11"));
  assert.deepEqual(newIn111, [["recording_window", "1.11"]]);
  assert.deepEqual(playerSessionIntroducedIn("recording_window"), PROTOCOL_VERSION_1_11);
  assert.deepEqual(CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_11);
});

test("recording_window пишется ровно задокументированным JSON и читается обратно", () => {
  for (const { json, value } of DOCUMENTED) {
    assert.deepEqual(encodePlayerSession(value), { ok: true, value: json });
    assert.deepEqual(decodePlayerSession(json), { ok: true, value });
  }
});

test("окно, вывернутое или за концом записи, отвергается и при записи, и при чтении - с точной причиной", () => {
  const cases: readonly [LiveSeekable, number, string][] = [
    [{ kind: "range", start_us: 51, end_us: 50 }, 100, "empty_range"],
    [{ kind: "range", start_us: 0, end_us: 101 }, 100, "number_out_of_range"],
  ];
  for (const [seekable, endUs, kind] of cases) {
    const written = encodePlayerSession(recordingWindow(1, endUs, seekable));
    assert.equal(written.ok ? "ok" : written.error.kind, kind);
    const range = seekable.kind === "range" ? seekable : undefined;
    const json = `{"type":"recording_window","generation":1,"end_us":${endUs},"seekable":{"kind":"range","start_us":${range?.start_us},"end_us":${range?.end_us}}}`;
    const decoded = decodePlayerSession(json);
    assert.equal(decoded.ok ? "ok" : decoded.error.kind, kind, json);
  }
});

test("форма recording_window осталась строгой", () => {
  const cases: readonly [string, string][] = [
    ['{"type":"recording_window","generation":1,"seekable":{"kind":"none"}}', "missing_field"],
    ['{"type":"recording_window","generation":1,"end_us":5}', "missing_field"],
    ['{"type":"recording_window","generation":1,"end_us":5,"seekable":null}', "wrong_value_type"],
    ['{"type":"recording_window","generation":1,"end_us":5,"seekable":{"kind":"dvr"}}', "unknown_value"],
    ['{"type":"recording_window","generation":1,"end_us":5,"seekable":{"kind":"none"},"edge_us":5}', "unknown_field"],
    ['{"type":"recording_window","generation":0,"end_us":5,"seekable":{"kind":"none"}}', "number_out_of_range"],
    ['{"type":"recording_window","generation":1,"end_us":5.5,"seekable":{"kind":"none"}}', "wrong_value_type"],
  ];
  for (const [json, kind] of cases) {
    const decoded = decodePlayerSession(json);
    assert.equal(decoded.ok ? "ok" : decoded.error.kind, kind, json);
  }
});

test("recording_window требует 1.11: с пиром 1.10 общая версия 1.10 и окно не уходит", () => {
  for (const { value } of DOCUMENTED) {
    assert.deepEqual(playerSessionIntroducedIn("recording_window"), PROTOCOL_VERSION_1_11);
    assert.deepEqual(playerSessionRequiredVersion(value), PROTOCOL_VERSION_1_11);
  }
  for (const [player, extension] of [
    [PROTOCOL_VERSION_1_11, PROTOCOL_VERSION_1_10],
    [PROTOCOL_VERSION_1_10, PROTOCOL_VERSION_1_11],
  ] as const) {
    const agreed = negotiateProtocolVersion(player, extension);
    assert.deepEqual(agreed, { ok: true, version: PROTOCOL_VERSION_1_10 });
    assert.equal(agreed.ok && versionIncludes(agreed.version, PROTOCOL_VERSION_1_11), false);
  }
  const both = negotiateProtocolVersion(PROTOCOL_VERSION_1_11, PROTOCOL_VERSION_1_11);
  assert.equal(both.ok && versionIncludes(both.version, PROTOCOL_VERSION_1_11), true);
});
