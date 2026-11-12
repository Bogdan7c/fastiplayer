// Настройка ожидания видео после перезагрузки страницы (сессия 16c, решение 305): чтение с мусором, запись, ввод.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  DEFAULT_PAGE_RELOAD_WAIT_MS,
  DEFAULT_PAGE_RELOAD_WAIT_SETTINGS,
  PAGE_RELOAD_WAIT_SETTINGS_KEY,
  pageReloadWaitMsOf,
  parsePageReloadWaitInput,
  parsePageReloadWaitSettings,
  readPageReloadWaitSettings,
  writePageReloadWaitSettings,
} from "../src/shared/page-reload-wait-settings.ts";
import { memoryStore } from "./background/auto-handoff-lists-fake-dom.ts";

describe("настройка ожидания после перезагрузки", () => {
  it("по умолчанию — 10 с; срок для фона — в миллисекундах", () => {
    assert.deepEqual(DEFAULT_PAGE_RELOAD_WAIT_SETTINGS, { seconds: 10 });
    assert.equal(DEFAULT_PAGE_RELOAD_WAIT_MS, 10_000);
    assert.equal(pageReloadWaitMsOf({ seconds: 0 }), 0);
    assert.equal(pageReloadWaitMsOf({ seconds: 60 }), 60_000);
  });

  it("мусор и числа вне 0..60 читаются как умолчание; 0 и 60 — годные", () => {
    for (const stored of [undefined, null, 7, "10", [], {}, { seconds: -1 }, { seconds: 61 }, { seconds: 2.5 }, { seconds: "5" }]) {
      assert.deepEqual(parsePageReloadWaitSettings(stored), DEFAULT_PAGE_RELOAD_WAIT_SETTINGS, JSON.stringify(stored));
    }
    assert.deepEqual(parsePageReloadWaitSettings({ seconds: 0 }), { seconds: 0 });
    assert.deepEqual(parsePageReloadWaitSettings({ seconds: 60 }), { seconds: 60 });
  });

  it("запись и чтение через хранилище; пустое хранилище — умолчание", async () => {
    const { store, values } = memoryStore();
    assert.deepEqual(await readPageReloadWaitSettings(store), DEFAULT_PAGE_RELOAD_WAIT_SETTINGS);
    await writePageReloadWaitSettings(store, { seconds: 25 });
    assert.deepEqual(values.get(PAGE_RELOAD_WAIT_SETTINGS_KEY), { seconds: 25 });
    assert.deepEqual(await readPageReloadWaitSettings(store), { seconds: 25 });
  });

  it("сбой хранилища не глотается: чтение отклоняется (умолчание выбирает вызывающий)", async () => {
    const { store, state } = memoryStore();
    state.failReads = true;
    await assert.rejects(readPageReloadWaitSettings(store), /read broke/);
  });

  it("ввод человека: целые секунды 0..60, остальное — `undefined`", () => {
    assert.equal(parsePageReloadWaitInput(" 15 "), 15);
    assert.equal(parsePageReloadWaitInput("0"), 0);
    assert.equal(parsePageReloadWaitInput("60"), 60);
    for (const text of ["", "61", "-1", "2.5", "ten", "1e1"]) assert.equal(parsePageReloadWaitInput(text), undefined, text);
  });
});
