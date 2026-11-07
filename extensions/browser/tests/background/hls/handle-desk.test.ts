// Стойка номерков: выдача, повтор, поколения, вытеснение, зарезервированный номерок.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createHandleDesk } from "../../../src/background/handoff/hls/handle-desk.ts";
import { REFUSED_RESOURCE_HANDLE } from "../../../src/background/handoff/hls/limits.ts";

describe("стойка номерков", () => {
  it("первый номерок - 0, дальше по порядку, запись находится по номерку", () => {
    const desk = createHandleDesk();
    assert.equal(desk.issue("https://a.example/root.m3u8", "playlist"), 0);
    assert.equal(desk.issue("https://a.example/seg1.ts", "media"), 1);
    assert.deepEqual(desk.lookup(0), { url: "https://a.example/root.m3u8", kind: "playlist" });
    assert.deepEqual(desk.lookup(1), { url: "https://a.example/seg1.ts", kind: "media" });
    assert.equal(desk.lookup(2), undefined);
  });

  it("тот же адрес в поколении - тот же номерок, таблица не растёт", () => {
    const desk = createHandleDesk();
    const first = desk.issue("https://a.example/live.m3u8", "playlist");
    assert.equal(desk.issue("https://a.example/live.m3u8", "playlist"), first);
    assert.equal(desk.size(), 1);
  });

  it("ссылка «плейлист» повышает вид, обратного понижения нет", () => {
    const desk = createHandleDesk();
    const handle = desk.issue("https://a.example/x", "media");
    assert.ok(handle !== undefined);
    desk.issue("https://a.example/x", "playlist");
    assert.equal(desk.lookup(handle)?.kind, "playlist");
    desk.issue("https://a.example/x", "media");
    assert.equal(desk.lookup(handle)?.kind, "playlist");
  });

  it("новое поколение забывает записи, но номерки не повторяются", () => {
    const desk = createHandleDesk();
    const old = desk.issue("https://a.example/old.ts", "media");
    assert.ok(old !== undefined);
    desk.beginGeneration();
    assert.equal(desk.lookup(old), undefined, "поздний запрос прошлого поколения - отказ");
    assert.equal(desk.size(), 0);
    const fresh = desk.issue("https://a.example/old.ts", "media");
    assert.equal(fresh, old + 1, "тот же адрес получает НОВЫЙ номерок");
    assert.equal(desk.lookup(old), undefined);
  });

  it("при переполнении вытесняются старейшие записи", () => {
    const desk = createHandleDesk({ maxEntries: 3, firstHandle: 0 });
    for (let index = 0; index < 5; index += 1) desk.issue(`https://a.example/${index}`, "media");
    assert.equal(desk.size(), 3);
    assert.equal(desk.lookup(0), undefined);
    assert.equal(desk.lookup(1), undefined);
    assert.equal(desk.lookup(2)?.url, "https://a.example/2");
    assert.equal(desk.lookup(4)?.url, "https://a.example/4");
    // Вытесненный адрес при повторной выдаче получает свежий номерок, а не старый.
    assert.equal(desk.issue("https://a.example/0", "media"), 5);
  });

  it("номерок «отказ» не выдаётся, счётчик останавливается перед ним", () => {
    const desk = createHandleDesk({ maxEntries: 10, firstHandle: REFUSED_RESOURCE_HANDLE - 2 });
    assert.equal(desk.issue("https://a.example/a", "media"), REFUSED_RESOURCE_HANDLE - 2);
    assert.equal(desk.issue("https://a.example/b", "media"), REFUSED_RESOURCE_HANDLE - 1);
    assert.equal(desk.issue("https://a.example/c", "media"), undefined);
    assert.equal(desk.lookup(REFUSED_RESOURCE_HANDLE), undefined);
    // Уже выданный адрес по-прежнему отдаёт свой номерок.
    assert.equal(desk.issue("https://a.example/b", "media"), REFUSED_RESOURCE_HANDLE - 1);
  });
});
