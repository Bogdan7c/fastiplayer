// Таблица действующей политики: настройка × метка плеера, и обмен нагрузкой «курьер → страница».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { computeEffectivePolicy } from "../../src/background/codec-policy/effective-policy.ts";
import {
  allCarrierRefusalSets,
  carrierFileName,
  decodeRefusedFamilies,
  encodeRefusedFamilies,
  refuse,
  type CodecPolicy,
  type CodecSetting,
  type VideoCodecFamily,
} from "../../src/shared/codec-policy.ts";

describe("computeEffectivePolicy", () => {
  const table: ReadonlyArray<readonly [CodecSetting, readonly VideoCodecFamily[], CodecPolicy]> = [
    ["off", [], { kind: "transparent" }],
    ["off", ["h264"], { kind: "transparent" }],
    ["always_h264", [], { kind: "refuse", families: ["av1", "vp9", "hevc", "vp8"] }],
    ["always_h264", ["h264", "av1"], { kind: "refuse", families: ["av1", "vp9", "hevc", "vp8"] }],
    // prefer_vp9: только AV1, VP8 не трогаем; метка не важна.
    ["prefer_vp9", [], { kind: "refuse", families: ["av1"] }],
    ["prefer_vp9", ["h264"], { kind: "refuse", families: ["av1"] }],
    // auto: метки нет или она пуста — прозрачно.
    ["auto", [], { kind: "transparent" }],
    // Метка {H.264} — отказать AV1, VP9, HEVC (VP8 не трогаем).
    ["auto", ["h264"], { kind: "refuse", families: ["av1", "vp9", "hevc"] }],
    // Умолчание игрока (VP9 первым): метка {H.264, HEVC, VP9} — отказать только AV1.
    ["auto", ["vp9", "hevc", "h264"], { kind: "refuse", families: ["av1"] }],
    ["auto", ["h264", "hevc"], { kind: "refuse", families: ["av1", "vp9"] }],
    // В метке нет H.264: он всё равно не отвергается.
    ["auto", ["vp9"], { kind: "refuse", families: ["av1", "hevc"] }],
    ["auto", ["hevc"], { kind: "refuse", families: ["av1", "vp9"] }],
    // Все семейства в метке — отказывать нечему.
    ["auto", ["av1", "hevc", "vp9", "h264"], { kind: "transparent" }],
    ["auto", ["av1", "hevc", "vp9"], { kind: "transparent" }],
  ];
  for (const [setting, label, expected] of table) {
    it(`${setting} + метка [${label.join(",")}]`, () => {
      assert.deepEqual(computeEffectivePolicy(setting, label), expected);
    });
  }
});

describe("нагрузка курьера и имена файлов", () => {
  it("кодирование и разбор взаимно обратны для всех 15 наборов; имена файлов уникальны", () => {
    const sets = allCarrierRefusalSets();
    assert.equal(sets.length, 15);
    for (const families of sets) assert.deepEqual(decodeRefusedFamilies(encodeRefusedFamilies(families)), families);
    assert.equal(new Set(sets.map(carrierFileName)).size, 15);
    assert.equal(carrierFileName(["av1", "vp9"]), "codec-policy-av1-vp9.js");
  });

  it("разбор строг: пустое, чужое (в том числе h264), повторы, не канонический порядок, не строка — отказ", () => {
    for (const bad of ["", "h264", "av1,", ",av1", "av1,av1", "vp9,av1", "AV1", "av1 ", 42, null, undefined, { toString: () => "av1" }, ["av1"], "a".repeat(100)]) {
      assert.equal(decodeRefusedFamilies(bad), undefined, JSON.stringify(bad));
    }
  });

  it("refuse приводит к канону и пустое делает прозрачным", () => {
    assert.deepEqual(refuse(["vp8", "av1", "av1"]), { kind: "refuse", families: ["av1", "vp8"] });
    assert.deepEqual(refuse([]), { kind: "transparent" });
  });
});
