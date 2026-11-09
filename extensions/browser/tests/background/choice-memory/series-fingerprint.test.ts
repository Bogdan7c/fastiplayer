// Отпечаток сериала (решение 281): ровно SHA-256 описанной формы, один сайт под разными адресами — один
// отпечаток, служебная страница — без памяти.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { describe, it } from "node:test";
import { pageSiteKeyOf, SERIES_FINGERPRINT_LABEL, seriesFingerprintOf } from "../../../src/background/choice-memory/series-fingerprint.ts";

describe("отпечаток сериала", () => {
  it("SHA-256 от «метка \\n сайт \\n сериал», строчные hex — та же формула, что считает стенд", async () => {
    const expected = createHash("sha256").update(`${SERIES_FINGERPRINT_LABEL}\nexample.com\nsome-show`, "utf8").digest("hex");
    assert.equal(await seriesFingerprintOf("example.com", "some-show"), expected);
    assert.match(expected, /^[0-9a-f]{64}$/u);
  });

  it("в отпечатке нет ни сайта, ни сериала; другой сериал или сайт — другой отпечаток", async () => {
    const fingerprint = await seriesFingerprintOf("example.com", "some-show");
    assert.ok(!fingerprint.includes("example") && !fingerprint.includes("show"));
    assert.notEqual(await seriesFingerprintOf("example.com", "other-show"), fingerprint);
    assert.notEqual(await seriesFingerprintOf("example.org", "some-show"), fingerprint);
  });

  it("ключ сайта: регистр, www и порт не важны; не веб-страница — памяти нет", () => {
    assert.equal(pageSiteKeyOf("https://WWW.Example.com:8443/series/1"), "example.com");
    assert.equal(pageSiteKeyOf("http://127.0.0.1:8080/page"), "127.0.0.1");
    assert.equal(pageSiteKeyOf("chrome://settings"), null);
    assert.equal(pageSiteKeyOf("file:///home/video.html"), null);
    assert.equal(pageSiteKeyOf(undefined), null);
  });
});
