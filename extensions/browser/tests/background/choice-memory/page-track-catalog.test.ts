// Каталог дорожек субтитров сеанса (14b2): номер → язык и подпись, поиск только среди дорожек текущей серии.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createPageTrackCatalog } from "../../../src/background/choice-memory/page-track-catalog.ts";

describe("каталог дорожек", () => {
  it("describe по номеру; повтор того же номера обновляет имя", () => {
    const catalog = createPageTrackCatalog();
    catalog.offered({ track: 1, language: "en", label: "English" }, 0);
    catalog.offered({ track: 1, language: "en", label: "English (CC)" }, 5);
    assert.deepEqual(catalog.describe(1), { track: 1, language: "en", label: "English (CC)" });
    assert.equal(catalog.describe(2), undefined);
  });

  it("find: дорожки прошлой серии не находятся, из подходящих — самая свежая", () => {
    const catalog = createPageTrackCatalog();
    catalog.offered({ track: 1, language: "en", label: "English" }, 0);
    catalog.offered({ track: 2, language: "en", label: "English" }, 10_000);
    catalog.offered({ track: 3, language: "en", label: "English" }, 10_500);
    assert.equal(catalog.find({ kind: "track", language: "en", label: "English" }, 9_000), 3);
    assert.equal(catalog.find({ kind: "track", language: "en", label: "English" }, 20_000), undefined);
  });

  it("find: точного совпадения нет — та же известная речь; язык неизвестен — только точное совпадение", () => {
    const catalog = createPageTrackCatalog();
    catalog.offered({ track: 4, language: "uk", label: "Українська (сайт)" }, 0);
    catalog.offered({ track: 5, language: null, label: "Subtitles" }, 0);
    assert.equal(catalog.find({ kind: "track", language: "uk", label: "Українська" }, 0), 4);
    assert.equal(catalog.find({ kind: "track", language: null, label: "Subtitles" }, 0), 5);
    assert.equal(catalog.find({ kind: "track", language: null, label: "Other" }, 0), undefined);
    assert.equal(catalog.find({ kind: "track", language: "de", label: "Deutsch" }, 0), undefined);
  });
});
