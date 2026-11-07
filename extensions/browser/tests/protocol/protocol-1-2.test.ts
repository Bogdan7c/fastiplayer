// Договоры протокола 1.2 (сессия 10a3, решение 232 - HLS live), которых не покрывает
// общий проход по manifest.json (его делает fixtures.test.ts для тех же фикстур `live_window`):
// - в 1.2 нового ровно одно сообщение - `live_window` (fixtures/message-versions.json);
// - собеседник с общей версией 1.1 и ниже `live_window` не получает;
// - инварианты окна `start_us <= end_us <= edge_us` проверяются и при записи;
// - договорные числа (частота окна).
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_2.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  MIN_LIVE_WINDOW_INTERVAL_MS,
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_1,
  PROTOCOL_VERSION_1_2,
  decodePlayerSession,
  encodePlayerSession,
  negotiateProtocolVersion,
  playerSessionIntroducedIn,
  versionIncludes,
  type LiveSeekable,
  type PlayerSessionMessage,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

/** Окно из примера договора: поколение 3, край 61 с. */
function contractWindow(seekable: LiveSeekable): PlayerSessionMessage {
  return { type: "live_window", generation: 3, edge_us: 61_000_000, seekable };
}

test("в 1.2 нового ровно одно сообщение - live_window", () => {
  const table = JSON.parse(readFileSync(join(FIXTURES_ROOT, "message-versions.json"), "utf8")) as Record<
    string,
    Record<string, string>
  >;
  const newer = Object.values(table)
    .flatMap((byType) => Object.entries(byType))
    .filter(([, version]) => version === "1.2")
    .map(([type]) => type);
  assert.deepEqual(newer, ["live_window"]);
});

test("live_window доходит только до собеседника, договорившегося на 1.2", () => {
  const introducedIn = playerSessionIntroducedIn("live_window");
  assert.deepEqual(introducedIn, PROTOCOL_VERSION_1_2);
  // Новый экстеншен со старым плеером: общая версия - старая, окно в неё не входит.
  for (const olderPlayer of [PROTOCOL_VERSION_1_1, PROTOCOL_VERSION_1_0]) {
    const agreed = negotiateProtocolVersion(olderPlayer, CURRENT_PROTOCOL_VERSION);
    assert.deepEqual(agreed, { ok: true, version: olderPlayer });
    assert.equal(agreed.ok && versionIncludes(agreed.version, introducedIn), false);
  }
  const agreed = negotiateProtocolVersion(PROTOCOL_VERSION_1_2, CURRENT_PROTOCOL_VERSION);
  assert.equal(agreed.ok && versionIncludes(agreed.version, introducedIn), true);
});

test("окна из договора пишутся ровно задокументированным JSON и читаются обратно", () => {
  const cases: [PlayerSessionMessage, string][] = [
    [
      contractWindow({ kind: "range", start_us: 40_000_000, end_us: 60_500_000 }),
      '{"type":"live_window","generation":3,"edge_us":61000000,"seekable":{"kind":"range","start_us":40000000,"end_us":60500000}}',
    ],
    [contractWindow({ kind: "none" }), '{"type":"live_window","generation":3,"edge_us":61000000,"seekable":{"kind":"none"}}'],
  ];
  for (const [message, json] of cases) {
    const written = encodePlayerSession(message);
    assert.ok(written.ok);
    assert.equal(written.value, json);
    assert.deepEqual(decodePlayerSession(json), { ok: true, value: message });
  }
});

test("окно, нарушающее start <= end <= edge, не пишется: виды ошибок как в Rust", () => {
  const inverted = encodePlayerSession(contractWindow({ kind: "range", start_us: 50_000_001, end_us: 50_000_000 }));
  assert.deepEqual(inverted, { ok: false, error: { kind: "empty_range", detail: "seekable.end_us" } });
  const beyondEdge = encodePlayerSession(contractWindow({ kind: "range", start_us: 40_000_000, end_us: 61_000_001 }));
  assert.deepEqual(beyondEdge, { ok: false, error: { kind: "number_out_of_range", detail: "seekable.end_us" } });
  // Граница включительно: окно в одну точку ровно на краю - честное окно.
  assert.equal(encodePlayerSession(contractWindow({ kind: "range", start_us: 61_000_000, end_us: 61_000_000 })).ok, true);
});

test("частота окна эфира - то же число, что в Rust", () => {
  assert.equal(MIN_LIVE_WINDOW_INTERVAL_MS, 1_000);
});
