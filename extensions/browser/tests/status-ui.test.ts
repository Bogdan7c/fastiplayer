// Тесты чистой логики: статус -> текст попапа, статус -> значок, тип-гард сообщений.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { badgeForStatus } from "../src/background/badge.ts";
import { describeStatus, HELP_URL } from "../src/popup/status-text.ts";
import type { BridgeStatus } from "../src/shared/bridge-status.ts";
import { isBridgeStatus, isStatusUpdateMessage } from "../src/shared/internal-messages.ts";

const connected: BridgeStatus = { state: "connected", playerVersion: "0.9.1", agreedProtocol: { major: 1, minor: 0 } };
const allStatuses: BridgeStatus[] = [
  { state: "unknown" },
  { state: "checking" },
  { state: "host_missing" },
  { state: "player_unavailable", reason: "player_unavailable" },
  { state: "player_unavailable", reason: "player_not_responding" },
  { state: "player_unavailable", reason: "player_shutting_down" },
  { state: "player_unavailable", reason: "no_reply" },
  { state: "incompatible", update: "extension" },
  { state: "incompatible", update: "player" },
  connected,
];

describe("текст попапа", () => {
  it("connected показывает версию плеера", () => {
    assert.equal(describeStatus(connected).headline, "Connected to Fastiplayer 0.9.1");
  });

  it("host_missing и player_unavailable: «not connected», инструкция и ссылка на справку", () => {
    for (const status of allStatuses.filter((s) => s.state === "host_missing" || s.state === "player_unavailable")) {
      const view = describeStatus(status);
      assert.equal(view.headline, "Fastiplayer is not connected");
      assert.ok(view.details.some((line) => line.includes("Settings › Browser integration")));
      assert.ok(view.details.some((line) => line.includes("Allow")));
      assert.equal(view.showHelpLink, true);
    }
    assert.match(HELP_URL, /docs\/browser-bridge\/README\.md$/);
  });

  it("incompatible называет, кого обновить", () => {
    assert.ok(describeStatus({ state: "incompatible", update: "extension" }).details[0]?.includes("extension"));
    assert.ok(describeStatus({ state: "incompatible", update: "player" }).details[0]?.includes("update Fastiplayer"));
  });

  it("unknown и checking не пугают человека", () => {
    for (const state of ["unknown", "checking"] as const) {
      const view = describeStatus({ state });
      assert.match(view.headline, /Checking/);
      assert.equal(view.showHelpLink, false);
    }
  });

  it("причины player_unavailable дают разные пояснения", () => {
    const texts = (["player_unavailable", "player_not_responding", "player_shutting_down"] as const).map(
      (reason) => describeStatus({ state: "player_unavailable", reason }).details[0],
    );
    assert.equal(new Set(texts).size, 3);
  });
});

describe("значок", () => {
  it("«!» для проблем, пусто для connected и unknown", () => {
    for (const status of allStatuses) {
      const { text } = badgeForStatus(status);
      if (status.state === "connected" || status.state === "unknown") assert.equal(text, "", status.state);
      else if (status.state === "checking") assert.notEqual(text, "!");
      else assert.equal(text, "!", status.state);
    }
  });
});

describe("тип-гард внутренних сообщений", () => {
  it("принимает все валидные статусы и обёртку status_update", () => {
    for (const status of allStatuses) {
      assert.equal(isBridgeStatus(status), true, status.state);
      assert.equal(isStatusUpdateMessage({ kind: "status_update", status }), true);
    }
  });

  it("отвергает мусор", () => {
    const garbage: unknown[] = [
      null, undefined, 42, "connected", [], {},
      { kind: "status_update" },
      { kind: "status_request", refresh: true },
      { kind: "status_update", status: { state: "nope" } },
      { kind: "status_update", status: { state: "unknown" }, extra: 1 },
      { kind: "status_update", status: { state: "unknown", extra: 1 } },
      { kind: "status_update", status: { state: "player_unavailable", reason: "weird" } },
      { kind: "status_update", status: { state: "incompatible", update: "both" } },
      { kind: "status_update", status: { state: "connected", playerVersion: 1, agreedProtocol: { major: 1, minor: 0 } } },
      { kind: "status_update", status: { state: "connected", playerVersion: "1", agreedProtocol: { major: -1, minor: 0 } } },
      { kind: "status_update", status: { state: "connected", playerVersion: "1", agreedProtocol: { major: 1 } } },
    ];
    for (const value of garbage) assert.equal(isStatusUpdateMessage(value), false, JSON.stringify(value));
  });
});
