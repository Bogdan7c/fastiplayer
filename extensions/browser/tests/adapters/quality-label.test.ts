// Подпись качества ступенью (15a): одинакова на всех роликах сайта, иначе память выбора (по подписи) промахнётся.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { resolutionStepLabel } from "../../src/adapters/capabilities/variants/quality-label.ts";

describe("подпись качества ступенью разрешения", () => {
  it("частота кадров и приписки сайта отбрасываются", () => {
    assert.equal(resolutionStepLabel("720p60"), "720p");
    assert.equal(resolutionStepLabel("1080p Premium"), "1080p");
    assert.equal(resolutionStepLabel("2160p60 HDR"), "2160p");
    assert.equal(resolutionStepLabel(" 480p "), "480p");
  });

  it("подпись уже ступенью не меняется; одна ступень с разной частотой даёт одну подпись", () => {
    assert.equal(resolutionStepLabel("360p"), "360p");
    assert.equal(resolutionStepLabel("720p30"), resolutionStepLabel("720p60"));
  });

  it("подпись без ступени в начале — как есть: «Auto», «Source», мусор", () => {
    assert.equal(resolutionStepLabel("Auto"), "Auto");
    assert.equal(resolutionStepLabel("Source (1080p60)"), "Source (1080p60)");
    assert.equal(resolutionStepLabel("p720"), "p720");
    assert.equal(resolutionStepLabel("720px wide"), "720px wide");
  });
});
