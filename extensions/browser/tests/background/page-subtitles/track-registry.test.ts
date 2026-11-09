// Номера дорожек сеанса и бюджет 32 МиБ.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createTrackRegistry } from "../../../src/background/handoff/page-subtitles/track-registry.ts";
import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES, MAX_PAGE_SUBTITLE_SESSION_BYTES, MAX_PAGE_SUBTITLE_TRACKS } from "../../../src/protocol/limits.ts";

describe("номера дорожек", () => {
  it("два ключа одного документа — разные номера; тот же ключ — тот же номер (замена)", () => {
    const registry = createTrackRegistry();
    const document = { frameId: 0, documentTag: "doc-a" };
    assert.deepEqual(registry.admit({ document, key: 1, byteLength: 10 }), { kind: "admitted", track: 1, replaced: false });
    assert.deepEqual(registry.admit({ document, key: 2, byteLength: 10 }), { kind: "admitted", track: 2, replaced: false });
    assert.deepEqual(registry.admit({ document, key: 1, byteLength: 20 }), { kind: "admitted", track: 1, replaced: true });
  });

  it("новый документ: тот же ключ получает НОВЫЙ номер, старые номера не повторяются", () => {
    const registry = createTrackRegistry();
    const first = { frameId: 0, documentTag: "doc-a" };
    registry.admit({ document: first, key: 1, byteLength: 10 });
    const second = { frameId: 0, documentTag: "doc-b" };
    assert.deepEqual(registry.admit({ document: second, key: 1, byteLength: 10 }), { kind: "admitted", track: 2, replaced: false });
    assert.deepEqual(registry.admit({ document: first, key: 1, byteLength: 12 }), { kind: "admitted", track: 1, replaced: true });
  });

  it("65-я дорожка — отказ, первые 64 живы и их можно заменять", () => {
    const registry = createTrackRegistry();
    const document = { frameId: 0, documentTag: "doc-a" };
    for (let key = 1; key <= MAX_PAGE_SUBTITLE_TRACKS; key += 1) {
      assert.deepEqual(registry.admit({ document, key, byteLength: 1 }), { kind: "admitted", track: key, replaced: false });
    }
    assert.deepEqual(registry.admit({ document, key: 65, byteLength: 1 }), { kind: "too_many_tracks" });
    assert.deepEqual(registry.admit({ document, key: 1, byteLength: 2 }), { kind: "admitted", track: 1, replaced: true });
    assert.equal(registry.usedBytes(), MAX_PAGE_SUBTITLE_TRACKS + 1);
  });
});

describe("бюджет сеанса", () => {
  const FOUR = MAX_PAGE_SUBTITLE_SESSION_BYTES / 4;
  it("сумма больше 32 МиБ — отказ, и состояние не тронуто", () => {
    const registry = createTrackRegistry();
    const document = { frameId: 0, documentTag: "doc-a" };
    for (let key = 1; key <= 4; key += 1) registry.admit({ document, key, byteLength: FOUR });
    assert.equal(registry.usedBytes(), MAX_PAGE_SUBTITLE_SESSION_BYTES);
    assert.deepEqual(registry.admit({ document, key: 5, byteLength: 1 }), { kind: "over_budget" });
    assert.equal(registry.usedBytes(), MAX_PAGE_SUBTITLE_SESSION_BYTES);
  });

  it("замена считает новую длину вместо старой: растущая дорожка в бюджет влезает", () => {
    const registry = createTrackRegistry();
    const document = { frameId: 0, documentTag: "doc-a" };
    for (let key = 1; key <= 4; key += 1) registry.admit({ document, key, byteLength: FOUR });
    // Дорожка 1 заменяется документом чуть меньше: освободилось место.
    assert.deepEqual(registry.admit({ document, key: 1, byteLength: FOUR - 100 }), { kind: "admitted", track: 1, replaced: true });
    assert.equal(registry.usedBytes(), MAX_PAGE_SUBTITLE_SESSION_BYTES - 100);
    // Замена, которая раздула бы сумму, отказана; прежний документ остаётся в счёте.
    assert.deepEqual(registry.admit({ document, key: 2, byteLength: FOUR + 101 }), { kind: "over_budget" });
    assert.equal(registry.usedBytes(), MAX_PAGE_SUBTITLE_SESSION_BYTES - 100);
  });

  it("один документ в потолок 8 МиБ допускается", () => {
    const registry = createTrackRegistry();
    assert.equal(registry.admit({ document: { frameId: 0, documentTag: "doc-a" }, key: 1, byteLength: MAX_PAGE_SUBTITLE_DOCUMENT_BYTES }).kind, "admitted");
  });
});
