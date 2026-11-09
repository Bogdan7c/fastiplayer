// Договор памяти выбора (14b2): терпимый разбор хранилища, строгая форма просьб страницы настроек, настройка объёма.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  parseChoiceMemoryReply,
  parseChoiceMemoryRequest,
  parseRememberedChoices,
  parseRememberedChoicesDocument,
} from "../src/shared/choice-memory-contract.ts";
import {
  DEFAULT_CHOICE_MEMORY_SETTINGS,
  parseChoiceMemoryCapacityInput,
  parseChoiceMemorySettings,
} from "../src/shared/choice-memory-settings.ts";

describe("разбор записи памяти", () => {
  it("битое поле выбрасывается одно, без годного времени — вся запись", () => {
    assert.deepEqual(parseRememberedChoices({ audio: "Dub B", quality: 7, subtitles: { kind: "off" }, used_at: 5 }), {
      audio: "Dub B",
      subtitles: { kind: "off" },
      used_at: 5,
    });
    assert.equal(parseRememberedChoices({ audio: "Dub B" }), undefined);
    assert.equal(parseRememberedChoices({ audio: "Dub B", used_at: -1 }), undefined);
  });

  it("субтитры: дорожка без языка и подписи не помнится, чужой вид — тоже", () => {
    assert.deepEqual(parseRememberedChoices({ subtitles: { kind: "track", language: "en", label: null }, used_at: 1 }), {
      subtitles: { kind: "track", language: "en", label: null },
      used_at: 1,
    });
    assert.deepEqual(parseRememberedChoices({ subtitles: { kind: "track", language: null, label: null }, used_at: 1 }), { used_at: 1 });
    assert.deepEqual(parseRememberedChoices({ subtitles: { kind: "auto" }, used_at: 1 }), { used_at: 1 });
  });

  it("документ: не объект, чужая версия, ключ не отпечаток — пустая память или без этой записи", () => {
    assert.deepEqual(parseRememberedChoicesDocument("junk").series, {});
    assert.deepEqual(parseRememberedChoicesDocument({ version: 2, series: {} }).series, {});
    assert.deepEqual(Object.keys(parseRememberedChoicesDocument({ version: 1, series: { "example.com": { used_at: 1 } } }).series), []);
  });
});

describe("просьбы страницы настроек", () => {
  it("ровно kind одного из двух видов; лишнее поле или чужой вид — не наше", () => {
    assert.deepEqual(parseChoiceMemoryRequest({ kind: "forget_remembered_choices" }), { kind: "forget_remembered_choices" });
    assert.deepEqual(parseChoiceMemoryRequest({ kind: "count_remembered_choices" }), { kind: "count_remembered_choices" });
    assert.equal(parseChoiceMemoryRequest({ kind: "forget_remembered_choices", all: true }), undefined);
    assert.equal(parseChoiceMemoryRequest({ kind: "hand_off_tab" }), undefined);
  });
  it("ответ фона: отрицательное или дробное число — не ответ", () => {
    assert.deepEqual(parseChoiceMemoryReply({ kind: "count", series: 3 }), { kind: "count", series: 3 });
    assert.equal(parseChoiceMemoryReply({ kind: "count", series: -1 }), undefined);
    assert.equal(parseChoiceMemoryReply({ kind: "count", series: 1.5 }), undefined);
  });
});

describe("настройка объёма", () => {
  it("вне диапазона, дробь, мусор — по умолчанию 500; ввод человека — только целое в диапазоне", () => {
    assert.deepEqual(parseChoiceMemorySettings({ capacity: 50 }), { capacity: 50 });
    assert.deepEqual(parseChoiceMemorySettings({ capacity: 5 }), DEFAULT_CHOICE_MEMORY_SETTINGS);
    assert.deepEqual(parseChoiceMemorySettings({ capacity: 99.5 }), DEFAULT_CHOICE_MEMORY_SETTINGS);
    assert.deepEqual(parseChoiceMemorySettings(null), { capacity: 500 });
    assert.equal(parseChoiceMemoryCapacityInput(" 800 "), 800);
    assert.equal(parseChoiceMemoryCapacityInput("9"), undefined);
    assert.equal(parseChoiceMemoryCapacityInput("5001"), undefined);
    assert.equal(parseChoiceMemoryCapacityInput("1e3"), undefined);
  });
});
