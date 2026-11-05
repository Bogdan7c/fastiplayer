// Политика Referer закачки: как браузер посчитал бы для этого адреса, но не больше сайта-источника.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DEFAULT_REFERRER_POLICY, effectiveReferrerPolicy, refererValueFor, type ReferrerPolicyName } from "../src/shared/referrer-policy.ts";

const ORIGIN = "https://site.example";
const VALUE = `${ORIGIN}/`;
const SAME_SITE_FILE = `${ORIGIN}/movie.mp4`;
const CROSS_SITE_FILE = "https://cdn.example/movie.mp4";
const DOWNGRADE_FILE = "http://cdn.example/movie.mp4";

describe("значение Referer по политике", () => {
  const TABLE: readonly (readonly [ReferrerPolicyName, string | null, string | null, string | null])[] = [
    // Политика,                        свой сайт, чужой сайт, https → http
    ["no-referrer", null, null, null],
    ["same-origin", VALUE, null, null],
    ["origin", VALUE, VALUE, VALUE],
    ["strict-origin", VALUE, VALUE, null],
    ["origin-when-cross-origin", VALUE, VALUE, VALUE],
    ["strict-origin-when-cross-origin", VALUE, VALUE, null],
    ["no-referrer-when-downgrade", VALUE, VALUE, null],
    ["unsafe-url", VALUE, VALUE, VALUE],
  ];

  for (const [policy, sameSite, crossSite, downgrade] of TABLE) {
    it(policy, () => {
      const referrer = { origin: ORIGIN, policy };
      assert.equal(refererValueFor(SAME_SITE_FILE, referrer), sameSite);
      assert.equal(refererValueFor(CROSS_SITE_FILE, referrer), crossSite);
      assert.equal(refererValueFor(DOWNGRADE_FILE, referrer), downgrade);
    });
  }

  it("страница на http: понижения нет", () => {
    assert.equal(refererValueFor(DOWNGRADE_FILE, { origin: "http://site.example", policy: "strict-origin" }), "http://site.example/");
  });

  it("другой порт — другой сайт для same-origin", () => {
    assert.equal(refererValueFor("https://site.example:8443/a.mp4", { origin: ORIGIN, policy: "same-origin" }), null);
  });
});

describe("политика документа и элемента", () => {
  it("без атрибута и <meta> — политика браузеров по умолчанию", () => {
    assert.equal(effectiveReferrerPolicy(null, []), DEFAULT_REFERRER_POLICY);
  });

  it("последний годный <meta> побеждает, незнакомый не сбрасывает действующую", () => {
    assert.equal(effectiveReferrerPolicy(null, ["origin", "same-origin"]), "same-origin");
    assert.equal(effectiveReferrerPolicy(null, ["origin", "что-то", ""]), "origin");
  });

  it("регистр и пробелы не важны", () => {
    assert.equal(effectiveReferrerPolicy(null, [" No-Referrer "]), "no-referrer");
    assert.equal(effectiveReferrerPolicy("  ORIGIN", []), "origin");
  });

  it("старые значения <meta> из стандарта HTML", () => {
    assert.equal(effectiveReferrerPolicy(null, ["never"]), "no-referrer");
    assert.equal(effectiveReferrerPolicy(null, ["always"]), "unsafe-url");
    assert.equal(effectiveReferrerPolicy(null, ["origin-when-crossorigin"]), "origin-when-cross-origin");
    assert.equal(effectiveReferrerPolicy(null, ["no-referrer", "default"]), DEFAULT_REFERRER_POLICY);
  });

  it("атрибут важнее документа, незнакомый атрибут не считается", () => {
    assert.equal(effectiveReferrerPolicy("same-origin", ["no-referrer"]), "same-origin");
    assert.equal(effectiveReferrerPolicy("never", ["origin"]), "origin", "старые значения только у <meta>");
  });
});
