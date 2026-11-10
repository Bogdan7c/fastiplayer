// Проверка кадра «фрейм -> фон» для вида `subtitles` (15a2): описание с меню субтитров и команда выбора.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parsePageVariantDescription, parseVariantCommandRequest } from "../src/shared/variant-watch-validation.ts";

const valid = {
  seriesKey: "s",
  episodeKey: "1",
  audio: [],
  quality: [],
  subtitles: [{ id: "t0_abc", label: "English", selected: true }],
  nextEpisode: false,
};

describe("описание меню страницы с субтитрами", () => {
  it("принимает меню субтитров и отдаёт его как есть", () => {
    assert.deepEqual(parsePageVariantDescription(valid), valid);
  });

  it("отвергает описание без поля subtitles, с лишним полем и с негодным пунктом", () => {
    const { subtitles: _omitted, ...without } = valid;
    assert.equal(parsePageVariantDescription(without), undefined);
    assert.equal(parsePageVariantDescription({ ...valid, extra: 1 }), undefined);
    assert.equal(parsePageVariantDescription({ ...valid, subtitles: [{ id: "", label: "x", selected: false }] }), undefined);
    assert.equal(parsePageVariantDescription({ ...valid, subtitles: "none" }), undefined);
    assert.equal(parsePageVariantDescription({ ...valid, subtitles: Array.from({ length: 257 }, (_unused, index) => ({ id: `t${index}`, label: "x", selected: false })) }), undefined);
  });
});

describe("команда выбора варианта", () => {
  const select = (variantKind: string) => ({ kind: "variant_command", command: { kind: "select", variant_kind: variantKind, variant_id: "t0_abc" } });

  it("subtitles принимается наравне с audio и quality; чужой вид - нет", () => {
    for (const variantKind of ["audio", "quality", "subtitles"]) {
      assert.deepEqual(parseVariantCommandRequest(select(variantKind))?.command, { kind: "select", variant_kind: variantKind, variant_id: "t0_abc" });
    }
    assert.equal(parseVariantCommandRequest(select("video")), undefined);
  });
});
