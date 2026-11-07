// Версии, возможности и коды закрытия сеанса.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CURRENT_PROTOCOL_VERSION,
  SESSION_CLOSE_CODES,
  capabilitiesToTokens,
  cheapCodecToken,
  closeCodeOf,
  closeReasonFromCode,
  decodeExtensionControl,
  decodePlayerControl,
  featureToken,
  negotiateProtocolVersion,
  parseCapabilityTokens,
  type ProtocolVersion,
  versionIncludes,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT, readFixtureBytes } from "./fixtures-support.ts";

const version = (major: number, minor: number): ProtocolVersion => ({ major, minor });

test("текущая версия протокола - 1.2 (HLS live, сессия 10a3)", () => {
  assert.deepEqual(CURRENT_PROTOCOL_VERSION, { major: 1, minor: 2 });
});

test("версия включает только свои и более старые minor того же major", () => {
  assert.equal(versionIncludes(version(1, 1), version(1, 0)), true);
  assert.equal(versionIncludes(version(1, 1), version(1, 1)), true);
  assert.equal(versionIncludes(version(1, 0), version(1, 1)), false);
  assert.equal(versionIncludes(version(2, 5), version(1, 0)), false);
  assert.equal(versionIncludes(version(1, 1), version(2, 0)), false);
});

test("один major: общая версия - меньшая minor, при любых ролях", () => {
  assert.deepEqual(negotiateProtocolVersion(version(1, 3), version(1, 1)), { ok: true, version: version(1, 1) });
  assert.deepEqual(negotiateProtocolVersion(version(1, 0), version(1, 7)), { ok: true, version: version(1, 0) });
  assert.deepEqual(negotiateProtocolVersion(version(1, 2), version(1, 2)), { ok: true, version: version(1, 2) });
});

test("разные major: обновить того, у кого major меньше", () => {
  assert.deepEqual(negotiateProtocolVersion(version(1, 9), version(2, 0)), { ok: false, update: "player" });
  assert.deepEqual(negotiateProtocolVersion(version(3, 0), version(2, 5)), { ok: false, update: "extension" });
});

test("hello экстеншена со следующим major разбирается, а договорённость требует обновить плеер", () => {
  const decoded = decodeExtensionControl(readFixtureBytes("control/valid/hello-extension-major-2.json"));
  assert.ok(decoded.ok && decoded.value.type === "hello");
  if (decoded.ok && decoded.value.type === "hello") {
    assert.deepEqual(negotiateProtocolVersion(CURRENT_PROTOCOL_VERSION, decoded.value.protocol), { ok: false, update: "player" });
  }
});

test("метки возможностей: знакомые распознаются, незнакомые feature.*/cheap_codec.* только считаются", () => {
  const parsed = parseCapabilityTokens([
    "cheap_codec.h264",
    "feature.page_geometry",
    "feature.from_the_future",
    "cheap_codec.av1",
    "cheap_codec.from_the_future",
    "something_else",
  ]);
  assert.deepEqual([...parsed.capabilities.cheapVideoCodecs].sort(), ["av1", "h264"]);
  assert.deepEqual([...parsed.capabilities.features], ["page_geometry"]);
  assert.equal(parsed.ignoredUnknownTokens, 3);
});

test("hello_accepted из фикстуры: метка из будущего пропущена, известный кодек сохранён", () => {
  const decoded = decodePlayerControl(readFixtureBytes("control/valid/hello-accepted.json"));
  assert.ok(decoded.ok && decoded.value.type === "hello_accepted");
  if (decoded.ok && decoded.value.type === "hello_accepted") {
    const parsed = parseCapabilityTokens(decoded.value.player.capabilities);
    assert.equal(parsed.ignoredUnknownTokens, 1);
    assert.equal(parsed.capabilities.cheapVideoCodecs.size, 1);
  }
});

test("метки возможностей: запись и разбор обратно дают то же множество, метки проходят проверку текста", () => {
  const everything = parseCapabilityTokens(["feature.page_geometry", "cheap_codec.h264", "cheap_codec.hevc", "cheap_codec.vp9", "cheap_codec.av1"]);
  const written = capabilitiesToTokens(everything.capabilities);
  assert.deepEqual(written, ["feature.page_geometry", "cheap_codec.h264", "cheap_codec.hevc", "cheap_codec.vp9", "cheap_codec.av1"]);
  const reparsed = parseCapabilityTokens(written);
  assert.deepEqual(reparsed.capabilities, everything.capabilities);
  assert.equal(reparsed.ignoredUnknownTokens, 0);
  assert.equal(featureToken("page_geometry"), "feature.page_geometry");
  assert.equal(cheapCodecToken("vp9"), "cheap_codec.vp9");
  // Записанные метки годятся для настоящего hello.
  const hello = JSON.stringify({ type: "hello", protocol: { major: 1, minor: 0 }, software_version: "0.1.0", capabilities: written });
  assert.equal(decodeExtensionControl(hello).ok, true);
});

test("коды закрытия сеанса: уникальные 4000-4009, обратное соответствие, чужие коды неизвестны", () => {
  const codes = Object.values(SESSION_CLOSE_CODES);
  assert.equal(new Set(codes).size, codes.length);
  for (const [reason, code] of Object.entries(SESSION_CLOSE_CODES)) {
    assert.ok(code >= 4000 && code <= 4999);
    assert.equal(closeReasonFromCode(code), reason);
  }
  assert.equal(closeCodeOf("protocol_violation"), 4003);
  assert.equal(closeCodeOf("replaced_by_another_tab"), 4001);
  assert.equal(closeReasonFromCode(1006), undefined);
  assert.equal(closeReasonFromCode(4999), undefined);
});

test("коды закрытия совпадают с Rust (SessionCloseReason::close_code)", () => {
  // Код читается прямо из исходника Rust: расхождение ловится без отдельной фикстуры.
  const rustSource = readFileSync(join(FIXTURES_ROOT, "..", "src", "session", "mod.rs"), "utf8");
  const pairs = [...rustSource.matchAll(/Self::(\w+) => (4\d{3}),/g)].map((match) => [match[1] ?? "", Number(match[2])] as const);
  assert.equal(pairs.length, Object.keys(SESSION_CLOSE_CODES).length);
  const toSnake = (name: string): string => name.replace(/[A-Z]/g, (letter, index: number) => (index === 0 ? "" : "_") + letter.toLowerCase());
  for (const [rustName, code] of pairs) {
    assert.equal((SESSION_CLOSE_CODES as Record<string, number>)[toSnake(rustName)], code, rustName);
  }
});
