// Ведро жетонов: основа лимитов страницы (решение 200).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createTokenBucket } from "../src/shared/token-bucket.ts";

describe("ведро жетонов", () => {
  it("отказ не тратит запас: большой кусок не мешает меньшему", () => {
    const bucket = createTokenBucket(100, () => 0);
    assert.equal(bucket.take(101), false);
    assert.equal(bucket.take(100), true);
    assert.equal(bucket.take(1), false);
  });

  it("запас капает со скоростью лимита и не копится сверх него", () => {
    const clock = { nowMs: 0 };
    const bucket = createTokenBucket(1_000, () => clock.nowMs);
    assert.equal(bucket.take(1_000), true);
    clock.nowMs = 250;
    assert.equal(bucket.take(251), false);
    assert.equal(bucket.take(250), true);
    clock.nowMs = 3_600_000;
    assert.equal(bucket.take(1_001), false, "час простоя не даёт больше секундного запаса");
  });

  it("часы назад не добавляют запаса", () => {
    const clock = { nowMs: 1_000 };
    const bucket = createTokenBucket(10, () => clock.nowMs);
    bucket.take(10);
    clock.nowMs = 0;
    assert.equal(bucket.take(1), false);
  });

  it("часы назад не замораживают ведро: запас снова капает от нового времени", () => {
    const clock = { nowMs: 1_000_000 };
    const bucket = createTokenBucket(1_000, () => clock.nowMs);
    assert.equal(bucket.take(1_000), true);
    clock.nowMs = 0;
    assert.equal(bucket.take(1), false, "шаг назад сам по себе запаса не даёт");
    clock.nowMs = 500;
    assert.equal(bucket.take(500), true, "полсекунды после шага назад — полсекунды запаса");
  });

  it("не конечная и отрицательная длина отвергается и не портит запас", () => {
    const bucket = createTokenBucket(100, () => 0);
    for (const amount of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -50]) {
      assert.equal(bucket.take(amount), false, String(amount));
    }
    assert.equal(bucket.take(100), true, "запас целый");
    assert.equal(bucket.take(1), false, "и не вырос от отрицательного числа");
  });

  it("часы вернули NaN: ведро не открывается навсегда", () => {
    const clock = { nowMs: 0 };
    const bucket = createTokenBucket(10, () => clock.nowMs);
    assert.equal(bucket.take(10), true);
    clock.nowMs = Number.NaN;
    assert.equal(bucket.take(1), false);
    clock.nowMs = 100;
    assert.equal(bucket.take(1), true, "часы починились — счёт продолжается");
    assert.equal(bucket.take(1), false);
  });
});
