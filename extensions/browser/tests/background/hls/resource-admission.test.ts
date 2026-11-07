// Допуск адреса: каждая причина отказа и обычный пропуск.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { FileAddressRule } from "../../../src/background/handoff/file-address-guard.ts";
import { admitsResourceUrl } from "../../../src/background/handoff/hls/resource-admission.ts";
import { MAX_FILE_SOURCE_URL_LENGTH } from "../../../src/shared/page-message-validation.ts";

const RULE: FileAddressRule = { pageHost: "video.example.com", setting: "block", consentedHost: null };

describe("допуск адреса ресурса HLS", () => {
  it("пропускает обычные http и https", () => {
    assert.equal(admitsResourceUrl("https://cdn.example.net/seg.ts?x=1", RULE), true);
    assert.equal(admitsResourceUrl("http://cdn.example.net/seg.ts", RULE), true);
  });

  const refusals: readonly [string, string][] = [
    ["пустой адрес", ""],
    ["относительный адрес", "/seg.ts"],
    ["чужая схема data", "data:text/plain,hi"],
    ["чужая схема skd", "skd://key"],
    ["схема file", "file:///etc/passwd"],
    ["имя пользователя", "https://user@cdn.example.net/a"],
    ["пароль", "https://user:pw@cdn.example.net/a"],
    ["слишком длинный адрес", `https://cdn.example.net/${"a".repeat(MAX_FILE_SOURCE_URL_LENGTH)}`],
    ["адрес-заглушка от страницы", "https://resource.fastiplayer-bridge.invalid/0"],
    ["заглушка в другом регистре", "https://RESOURCE.fastiplayer-bridge.invalid/0"],
    ["локальный адрес при «блокировать»", "http://192.168.1.5/seg.ts"],
    ["localhost при «блокировать»", "http://localhost:8080/seg.ts"],
  ];
  for (const [name, url] of refusals) {
    it(`отказывает: ${name}`, () => {
      assert.equal(admitsResourceUrl(url, RULE), false);
    });
  }

  it("локальный адрес с согласием на этот хост проходит, на другой - нет", () => {
    const consented: FileAddressRule = { pageHost: "video.example.com", setting: "ask", consentedHost: "192.168.1.5" };
    assert.equal(admitsResourceUrl("http://192.168.1.5/seg.ts", consented), true);
    assert.equal(admitsResourceUrl("http://192.168.1.6/seg.ts", consented), false);
  });
});
