// Доля элемента на экране (`visibleShare`): по ней тень решает, что ролик ленты уходит с экрана (16b2).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { visibleShare } from "../../src/page/video-candidates.ts";

const VIEWPORT = { width: 1280, height: 720 } as const;

describe("доля элемента на экране", () => {
  it("элемент целиком в окне — 1", () => {
    assert.equal(visibleShare({ left: 100, top: 100, right: 740, bottom: 460 }, VIEWPORT), 1);
  });

  it("половина высоты за верхним краем — 0,5", () => {
    assert.equal(visibleShare({ left: 0, top: -180, right: 640, bottom: 180 }, VIEWPORT), 0.5);
  });

  it("срезан и по высоте, и по ширине — произведение долей", () => {
    // По высоте в окне 360 из 720 px (0,5), по ширине 640 из 1280 px (0,5).
    assert.equal(visibleShare({ left: 640, top: 360, right: 1920, bottom: 1080 }, VIEWPORT), 0.25);
  });

  it("за окном — 0", () => {
    assert.equal(visibleShare({ left: 0, top: 5_000, right: 640, bottom: 5_360 }, VIEWPORT), 0);
    assert.equal(visibleShare({ left: -700, top: 0, right: -60, bottom: 360 }, VIEWPORT), 0);
  });

  it("нулевой прямоугольник (элемент вне документа) — 0, без деления на ноль", () => {
    assert.equal(visibleShare({ left: 0, top: 0, right: 0, bottom: 0 }, VIEWPORT), 0);
  });

  it("маленький элемент, видный целиком, — 1, большой, видный наполовину, — 0,5: площадь в пикселях их бы спутала", () => {
    const small = visibleShare({ left: 0, top: 0, right: 160, bottom: 90 }, VIEWPORT);
    const largeHalf = visibleShare({ left: 0, top: 360, right: 1280, bottom: 1080 }, VIEWPORT);
    assert.equal(small, 1);
    assert.equal(largeHalf, 0.5);
  });
});
