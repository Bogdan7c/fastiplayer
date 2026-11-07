// Договоры протокола 1.1 (сессия 10a1), которых нет в manifest.json:
// - fixtures/message-versions.json - в какой версии появилось каждое сообщение сеанса;
// - fixtures/resource-addresses.json - верные и неверные адреса-заглушки номерков HLS.
// Те же файлы проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_1.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  MAX_CONCURRENT_RESOURCE_FETCHES,
  MAX_RESOURCE_FETCH_BYTES,
  decodePlayerSession,
  encodePlayerSession,
  formatResourceAddress,
  pageSessionIntroducedIn,
  parseResourceAddress,
  playerSessionIntroducedIn,
  type PageSessionMessage,
  type PlayerSessionMessage,
} from "../../src/protocol/index.ts";
import { pageSessionCodec } from "../../src/protocol/session/page.ts";
import { playerSessionCodec } from "../../src/protocol/session/player.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

function readJson<T>(name: string): T {
  return JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as T;
}

const versionText = (version: { major: number; minor: number }): string => `${version.major}.${version.minor}`;

test("таблица версий сообщений совпадает с общей фикстурой и покрывает все типы кодеков", () => {
  const table = readJson<{ page_session: Record<string, string>; player_session: Record<string, string> }>("message-versions.json");
  assert.deepEqual(Object.keys(table.page_session).sort(), [...pageSessionCodec.knownTags].sort());
  assert.deepEqual(Object.keys(table.player_session).sort(), [...playerSessionCodec.knownTags].sort());
  for (const [type, version] of Object.entries(table.page_session)) {
    assert.equal(versionText(pageSessionIntroducedIn(type as PageSessionMessage["type"])), version, type);
  }
  for (const [type, version] of Object.entries(table.player_session)) {
    assert.equal(versionText(playerSessionIntroducedIn(type as PlayerSessionMessage["type"])), version, type);
  }
});

test("адреса-заглушки: верные дают свой номерок и пишутся обратно точно, неверные отвергаются", () => {
  const addresses = readJson<{ valid: { address: string; handle: number }[]; invalid: string[] }>("resource-addresses.json");
  assert.ok(addresses.valid.length >= 5 && addresses.invalid.length >= 30);
  for (const { address, handle } of addresses.valid) {
    assert.equal(parseResourceAddress(address), handle, address);
    assert.equal(formatResourceAddress(handle), address);
  }
  const accepted = addresses.invalid.filter((address) => parseResourceAddress(address) !== undefined);
  assert.deepEqual(accepted, []);
});

test("лимиты протокола 1.1 - те же числа, что в Rust", () => {
  assert.equal(MAX_RESOURCE_FETCH_BYTES, 134_217_728);
  assert.equal(MAX_CONCURRENT_RESOURCE_FETCHES, 8);
});

test("кусок длиннее потолка ответа: запись отказывает, разбор тоже - number_out_of_range", () => {
  const contradictory: PlayerSessionMessage = {
    type: "fetch_resource",
    request_id: 1,
    resource: 2,
    byte_range: { kind: "slice", offset: 0, length: 2048 },
    max_bytes: 1024,
  };
  const written = encodePlayerSession(contradictory);
  assert.equal(written.ok, false);
  assert.equal(written.ok ? "" : written.error.kind, "number_out_of_range");
  const fitting = encodePlayerSession({ ...contradictory, max_bytes: 2048 });
  assert.ok(fitting.ok);
  assert.equal(
    fitting.value,
    '{"type":"fetch_resource","request_id":1,"resource":2,"byte_range":{"kind":"slice","offset":0,"length":2048},"max_bytes":2048}',
  );
  assert.equal(decodePlayerSession(fitting.value).ok, true);
});
