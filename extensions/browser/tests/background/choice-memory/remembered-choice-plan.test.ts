// Правило памяти выбора (решения 284, 287): что сделать с запомненным вариантом на текущей серии.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { planRememberedVariant, selectionOf, type RememberedVariantFacts } from "../../../src/background/choice-memory/remembered-choice-plan.ts";
import type { VariantMenu } from "../../../src/protocol/index.ts";

const DUBS: VariantMenu = {
  kind: "audio",
  options: [
    { id: "dub-a", label: "Dub A", selection: "selected" },
    { id: "dub-b", label: "Dub B", selection: "available" },
  ],
};
const QUALITIES: VariantMenu = {
  kind: "quality",
  options: [
    { id: "auto", label: "Auto", selection: "selected" },
    { id: "q720", label: "720p", selection: "available" },
  ],
};

const audioFacts = (overrides: Partial<RememberedVariantFacts> = {}): RememberedVariantFacts => ({
  variantKind: "audio",
  rememberedLabel: "Dub B",
  selected: selectionOf(DUBS, new Set()),
  menu: DUBS,
  timing: "near_start",
  ...overrides,
});

describe("озвучка — только у начала серии", () => {
  it("у начала — выбрать пункт с запомненной подписью", () => {
    assert.deepEqual(planRememberedVariant(audioFacts()), { kind: "select", variantId: "dub-b" });
  });
  it("серия уже идёт — не трогать (сайт начал бы серию заново)", () => {
    assert.deepEqual(planRememberedVariant(audioFacts({ timing: "under_way" })), { kind: "settled", reason: "episode_under_way" });
  });
  it("позиция неизвестна — ждать, а не гадать", () => {
    assert.deepEqual(planRememberedVariant(audioFacts({ timing: "position_unknown" })), { kind: "wait" });
  });
});

describe("качество — в любой момент (решение 287)", () => {
  const qualityFacts = (timing: RememberedVariantFacts["timing"]): RememberedVariantFacts => ({
    variantKind: "quality",
    rememberedLabel: "720p",
    selected: selectionOf(QUALITIES, new Set(["auto"])),
    menu: QUALITIES,
    timing,
  });
  it("и у начала, и посреди серии, и до часов плеера; «Auto» сайта не мешает", () => {
    for (const timing of ["near_start", "under_way", "position_unknown"] as const) {
      assert.deepEqual(planRememberedVariant(qualityFacts(timing)), { kind: "select", variantId: "q720" });
    }
  });
});

describe("когда делать нечего", () => {
  it("уже стоит запомненное, на странице такого нет, ничего не запомнено, выбор страницы ещё не виден", () => {
    assert.deepEqual(planRememberedVariant(audioFacts({ rememberedLabel: "Dub A" })), { kind: "settled", reason: "already_on_page" });
    assert.deepEqual(planRememberedVariant(audioFacts({ rememberedLabel: "Dub Z" })), { kind: "settled", reason: "not_on_page" });
    assert.deepEqual(planRememberedVariant(audioFacts({ rememberedLabel: undefined })), { kind: "settled", reason: "nothing_remembered" });
    assert.deepEqual(planRememberedVariant(audioFacts({ selected: undefined })), { kind: "wait" });
  });
  it("selectionOf: выбранный пункт с пометкой «Auto»; ничего не выбрано — undefined", () => {
    assert.deepEqual(selectionOf(QUALITIES, new Set(["auto"])), { id: "auto", label: "Auto", automatic: true });
    assert.equal(selectionOf({ kind: "audio", options: [{ id: "x", label: "X", selection: "available" }] }, new Set()), undefined);
  });
});
