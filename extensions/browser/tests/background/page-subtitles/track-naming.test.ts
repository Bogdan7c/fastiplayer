// Имя дорожки по правилам протокола 1.5: язык, подпись, автосубтитры.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { protocolNamingOf, sanitizeSubtitleLabel } from "../../../src/background/handoff/page-subtitles/track-naming.ts";
import { utf8ByteLength } from "../../../src/protocol/index.ts";
import { MAX_SUBTITLE_LABEL_BYTES } from "../../../src/protocol/limits.ts";

describe("язык дорожки", () => {
  it("годный тег проходит как есть, регистр не меняется", () => {
    assert.deepEqual(protocolNamingOf({ language: "pt-BR", label: null, auto_generated: false }).language, { kind: "known", tag: "pt-BR" });
  });

  for (const broken of ["en_US", "-en", "en--US", "English language tag that is way too long for BCP", "ру́сский", "e n"]) {
    it(`неверный тег «${broken}» — язык не назван`, () => {
      assert.deepEqual(protocolNamingOf({ language: broken, label: null, auto_generated: false }).language, { kind: "unknown" });
    });
  }

  it("язык не назван страницей — unknown", () => {
    assert.deepEqual(protocolNamingOf({ language: null, label: null, auto_generated: false }).language, { kind: "unknown" });
  });
});

describe("подпись дорожки", () => {
  it("обычная подпись остаётся, подписи нет — none", () => {
    assert.deepEqual(protocolNamingOf({ language: "en", label: "English", auto_generated: false }).label, { kind: "text", text: "English" });
    assert.deepEqual(protocolNamingOf({ language: "en", label: null, auto_generated: false }).label, { kind: "none" });
  });

  it("управляющие и bidi символы убраны, разрывы строк стали одним пробелом", () => {
    assert.equal(sanitizeSubtitleLabel("  Eng‮lish\u0000\n\n  (CC)⁧ "), "English (CC)");
  });

  it("подпись из одних запрещённых символов и пробелов — none", () => {
    assert.deepEqual(protocolNamingOf({ language: "en", label: " ‮\n ", auto_generated: false }).label, { kind: "none" });
  });

  it("длинная подпись обрезана до 256 байт по границе символа, а не посреди него", () => {
    // Символ в 3 байта: 256 не делится на 3, значит обрезка обязана остановиться на 85 символах (255 байт).
    const label = sanitizeSubtitleLabel("字".repeat(300));
    assert.equal(utf8ByteLength(label), 255);
    assert.equal([...label].length, 85);
    assert.ok(utf8ByteLength(label) <= MAX_SUBTITLE_LABEL_BYTES);
    assert.equal(label, "字".repeat(85));
  });

  it("эмодзи в 4 байта не режется пополам на границе", () => {
    const label = sanitizeSubtitleLabel("a".repeat(254) + "😀");
    assert.equal(label, "a".repeat(254));
  });
});

describe("автосубтитры (решение 276)", () => {
  it("без подписи страницы: «English (auto-generated)»", () => {
    assert.deepEqual(protocolNamingOf({ language: "en", label: null, auto_generated: true }).label, { kind: "text", text: "English (auto-generated)" });
    assert.deepEqual(protocolNamingOf({ language: "uk", label: null, auto_generated: true }).label, { kind: "text", text: "Ukrainian (auto-generated)" });
  });

  it("подпись страницы главнее: сочинять нечего", () => {
    assert.deepEqual(protocolNamingOf({ language: "en", label: "English (CC)", auto_generated: true }).label, { kind: "text", text: "English (CC)" });
  });

  it("язык неизвестен или негодный — «Auto-generated»", () => {
    assert.deepEqual(protocolNamingOf({ language: null, label: null, auto_generated: true }).label, { kind: "text", text: "Auto-generated" });
    assert.deepEqual(protocolNamingOf({ language: "en_US", label: null, auto_generated: true }).label, { kind: "text", text: "Auto-generated" });
  });

  it("тег прошёл правило протокола, но у браузера названия нет — «Auto-generated», а не сырой тег", () => {
    assert.deepEqual(protocolNamingOf({ language: "zzzzzzzz", label: null, auto_generated: true }).label, { kind: "text", text: "Auto-generated" });
  });

  it("не автосубтитры без подписи — none, ничего не сочиняется", () => {
    assert.deepEqual(protocolNamingOf({ language: "en", label: null, auto_generated: false }).label, { kind: "none" });
  });
});
