// Шаг взвода возврата страницы (сессия 11b2): чистое решение по фактам элемента.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseIncomingPageMessage } from "../../src/page/page-command-guard.ts";
import { MAX_RETURN_SEEKS, pageReturnStep, RETURN_POSITION_SETTLE_S, type ReturnVideoFacts } from "../../src/page/page-return-step.ts";
import { PAGE_RETURN_DURATION_TOLERANCE_S, type PageReturnTarget } from "../../src/shared/handoff-contract.ts";

const FILM: PageReturnTarget = { kind: "recorded", duration_s: 1_000, start_s: 300 };
const READY: ReturnVideoFacts = { readyState: 4, durationS: 1_000, currentTimeS: 0, seeking: false, seeksIssued: 0 };

describe("шаг взвода возврата", () => {
  it("без метаданных — ждать; длину ещё не сверить", () => {
    assert.deepEqual(pageReturnStep({ ...READY, readyState: 0, durationS: Number.NaN }, FILM), { kind: "wait" });
  });

  it("длина сверяется с допуском ± PAGE_RETURN_DURATION_TOLERANCE_S, граница включительно", () => {
    assert.deepEqual(pageReturnStep({ ...READY, durationS: 1_000 + PAGE_RETURN_DURATION_TOLERANCE_S }, FILM), { kind: "seek_to", seconds: 300 });
    assert.deepEqual(pageReturnStep({ ...READY, durationS: 1_000 - PAGE_RETURN_DURATION_TOLERANCE_S - 0.01 }, FILM), { kind: "not_this_video" });
    assert.deepEqual(pageReturnStep({ ...READY, durationS: Number.POSITIVE_INFINITY }, FILM), { kind: "not_this_video" }, "эфир — не запись");
  });

  it("перемотка: сначала на секунду плеера, во время перемотки ждать, на месте — тень", () => {
    assert.deepEqual(pageReturnStep(READY, FILM), { kind: "seek_to", seconds: 300 });
    assert.deepEqual(pageReturnStep({ ...READY, seeking: true, seeksIssued: 1 }, FILM), { kind: "wait" });
    assert.deepEqual(pageReturnStep({ ...READY, currentTimeS: 300 + RETURN_POSITION_SETTLE_S, seeksIssued: 1 }, FILM), { kind: "hold" });
  });

  it("сайт сбрасывает позицию: после MAX_RETURN_SEEKS попыток тень там, где страница", () => {
    assert.deepEqual(pageReturnStep({ ...READY, seeksIssued: MAX_RETURN_SEEKS - 1 }, FILM), { kind: "seek_to", seconds: 300 });
    assert.deepEqual(pageReturnStep({ ...READY, seeksIssued: MAX_RETURN_SEEKS }, FILM), { kind: "hold" });
  });

  it("эфир: только видео без конца шкалы, без перемотки", () => {
    assert.deepEqual(pageReturnStep({ ...READY, durationS: Number.POSITIVE_INFINITY }, { kind: "live" }), { kind: "hold" });
    assert.deepEqual(pageReturnStep({ ...READY, durationS: Number.NaN }, { kind: "live" }), { kind: "hold" });
    assert.deepEqual(pageReturnStep(READY, { kind: "live" }), { kind: "not_this_video" });
  });
});

describe("взвод возврата в MAIN-мире: строгий разбор", () => {
  it("цель проходит целиком, битая цель — команда отвергнута, обычный взвод прежний", () => {
    const withTarget = { kind: "arm_next_video", lease_ms: 3_000, page_return: { kind: "recorded", duration_s: 10, start_s: 2 } };
    assert.deepEqual(parseIncomingPageMessage(withTarget), withTarget);
    assert.deepEqual(parseIncomingPageMessage({ kind: "arm_next_video", lease_ms: 3_000 }), { kind: "arm_next_video", lease_ms: 3_000 });
    assert.equal(parseIncomingPageMessage({ kind: "arm_next_video", lease_ms: 3_000, page_return: { kind: "recorded", duration_s: 10 } }), null);
    assert.equal(parseIncomingPageMessage({ kind: "arm_next_video", lease_ms: 3_000, page_return: { kind: "live" }, extra: true }), null);
  });
});
