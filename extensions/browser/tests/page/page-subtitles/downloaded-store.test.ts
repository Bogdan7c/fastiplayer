// Склад скачанных документов субтитров (13b2): потолки, выселение, одинаковое содержимое, «последний = showing».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DownloadedSubtitleStore, MAX_STORED_SUBTITLE_BYTES, MAX_STORED_SUBTITLE_DOCUMENTS, type SubtitleDocumentCandidate } from "../../../src/page/page-subtitles/downloaded-store.ts";
import { SubtitleKeyAllocator } from "../../../src/page/page-subtitles/subtitle-keys.ts";

const NAMING = { language: "en", label: null, auto_generated: false } as const;

/** Документ заданного размера, различимый по `seed` (первые байты), чтобы отпечатки не совпали. */
function document(seed: number, byteLength = 16): SubtitleDocumentCandidate {
  const bytes = new Uint8Array(byteLength).fill(0x61);
  new DataView(bytes.buffer).setUint32(0, seed);
  return { naming: NAMING, formatHint: "web_vtt", bytes };
}

describe("склад скачанных документов", () => {
  it("новый документ получает ключ; самый свежий — showing, остальные — not_showing", () => {
    const store = new DownloadedSubtitleStore(new SubtitleKeyAllocator());
    const first = store.put(document(1));
    const second = store.put(document(2));
    assert.ok(first !== null && second !== null);
    assert.notEqual(first.key, second.key);
    assert.equal(store.displayOf(first.key), "not_showing");
    assert.equal(store.displayOf(second.key), "showing");
    assert.deepEqual(store.snapshot().map((stored) => stored.key), [first.key, second.key], "от старого к свежему");
  });

  it("одинаковое содержимое — тот же ключ, и документ снова самый свежий (showing)", () => {
    const store = new DownloadedSubtitleStore(new SubtitleKeyAllocator());
    const first = store.put(document(1));
    const second = store.put(document(2));
    const firstAgain = store.put(document(1));
    assert.ok(first !== null && second !== null && firstAgain !== null);
    assert.equal(firstAgain.key, first.key, "тот же ключ");
    assert.equal(store.documentCount, 2, "второй копии на складе нет");
    assert.equal(store.displayOf(first.key), "showing");
    assert.equal(store.displayOf(second.key), "not_showing");
    assert.equal(store.totalBytes, 32);
  });

  it("разные документы одной длины не сливаются", () => {
    const store = new DownloadedSubtitleStore(new SubtitleKeyAllocator());
    const left = store.put({ naming: NAMING, formatHint: "srt", bytes: new Uint8Array([1, 2, 3, 4]) });
    const right = store.put({ naming: NAMING, formatHint: "srt", bytes: new Uint8Array([1, 2, 3, 5]) });
    assert.ok(left !== null && right !== null);
    assert.notEqual(left.key, right.key);
  });

  it("потолок по числу документов: старые выселяются первыми", () => {
    const store = new DownloadedSubtitleStore(new SubtitleKeyAllocator());
    const stored = [];
    for (let seed = 1; seed <= MAX_STORED_SUBTITLE_DOCUMENTS + 3; seed += 1) stored.push(store.put(document(seed)));
    assert.equal(store.documentCount, MAX_STORED_SUBTITLE_DOCUMENTS);
    const keptKeys = store.snapshot().map((entry) => entry.key);
    assert.deepEqual(keptKeys, stored.slice(3).map((entry) => entry?.key), "первые три выселены, порядок сохранён");
  });

  it("потолок по байтам: старые выселяются, пока склад не уложится; свежий документ остаётся", () => {
    const store = new DownloadedSubtitleStore(new SubtitleKeyAllocator());
    const eightMiB = 8 * 1024 * 1024;
    const a = store.put(document(1, eightMiB));
    const b = store.put(document(2, eightMiB));
    assert.equal(store.documentCount, 2);
    assert.equal(store.totalBytes, MAX_STORED_SUBTITLE_BYTES, "ровно на потолке — ещё помещается");
    const c = store.put(document(3, 1024));
    assert.ok(a !== null && b !== null && c !== null);
    assert.deepEqual(store.snapshot().map((entry) => entry.key), [b.key, c.key], "самый старый выселен");
    assert.equal(store.totalBytes, eightMiB + 1024);
  });

  it("повтор документа не раздувает склад (учёт байт не удваивается)", () => {
    const store = new DownloadedSubtitleStore(new SubtitleKeyAllocator());
    for (let round = 0; round < 5; round += 1) store.put(document(7, 1000));
    assert.equal(store.documentCount, 1);
    assert.equal(store.totalBytes, 1000);
  });
});
