// Отпечаток подписи пункта меню: стандартный FNV-1a 32 бита в base36 (вынос из адаптера субтитров YouTube, 15b).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { optionFingerprintOf } from "../../src/adapters/capabilities/variants/option-fingerprint.ts";

describe("отпечаток подписи пункта", () => {
  it("совпадает с опорными значениями FNV-1a 32 бита", () => {
    // Опорные значения FNV-1a: "" → 0x811c9dc5, "a" → 0xe40c292c.
    assert.equal(optionFingerprintOf(""), (0x811c9dc5).toString(36));
    assert.equal(optionFingerprintOf("a"), (0xe40c292c).toString(36));
  });

  it("различает соседние подписи и годится в id протокола", () => {
    const first = optionFingerprintOf("Серія 1");
    const second = optionFingerprintOf("Серія 2");
    assert.notEqual(first, second);
    assert.match(first, /^[0-9a-z]{1,7}$/u);
  });
});
