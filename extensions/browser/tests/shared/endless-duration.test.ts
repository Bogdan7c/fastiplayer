// Длительность «без конца» (16d1, решение 319): одно правило у всех, кто решает «эфир или запись».
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { finiteDurationSeconds } from "../../src/page/candidate-facts.ts";
import { hlsStreamOfDuration } from "../../src/page/hls-stream-kind.ts";
import { durationOf } from "../../src/page/mse-values.ts";
import { pageReturnStep } from "../../src/page/page-return-step.ts";
import { ENDLESS_DURATION_THRESHOLD_SECONDS, isEndlessDurationSeconds } from "../../src/shared/endless-duration.ts";

/** Длительность эфира Twitch у элемента в Firefox (замер 16d1): «максимум» Firefox в микросекундах. */
const TWITCH_FIREFOX_DURATION_S = 9_223_372_036_854.775;
/** Круглосуточная запись YouTube — до 12 ч: это запись, не эфир. */
const TWELVE_HOURS_S = 12 * 60 * 60;

describe("endless duration", () => {
  it("is infinity or longer than a year", () => {
    assert.equal(isEndlessDurationSeconds(Number.POSITIVE_INFINITY), true);
    assert.equal(isEndlessDurationSeconds(TWITCH_FIREFOX_DURATION_S), true);
    assert.equal(isEndlessDurationSeconds(ENDLESS_DURATION_THRESHOLD_SECONDS + 1), true);
    assert.equal(isEndlessDurationSeconds(ENDLESS_DURATION_THRESHOLD_SECONDS), false);
    assert.equal(isEndlessDurationSeconds(TWELVE_HOURS_S), false);
    assert.equal(isEndlessDurationSeconds(Number.NaN), false);
  });

  it("tells the player that the Twitch stream in Firefox is live, not a recording of centuries", () => {
    assert.deepEqual(durationOf(TWITCH_FIREFOX_DURATION_S), { kind: "live" });
    assert.deepEqual(durationOf(TWELVE_HOURS_S), { kind: "finite", duration_us: TWELVE_HOURS_S * 1_000_000 });
    assert.deepEqual(durationOf(Number.NaN), { kind: "unknown" });
  });

  it("puts a live shadow on native HLS with the endless duration", () => {
    assert.equal(hlsStreamOfDuration(TWITCH_FIREFOX_DURATION_S), "live");
    assert.equal(hlsStreamOfDuration(TWELVE_HOURS_S), "vod");
    assert.equal(hlsStreamOfDuration(Number.NaN), null);
  });

  it("gives the background no film length for an endless video", () => {
    assert.equal(finiteDurationSeconds(TWITCH_FIREFOX_DURATION_S), null);
    assert.equal(finiteDurationSeconds(TWELVE_HOURS_S), TWELVE_HOURS_S);
  });

  it("returns a live stream to the page when its element shows the endless duration", () => {
    // Метаданные есть, элемент не перематывается: решает только длина.
    const facts = { readyState: 4, durationS: TWITCH_FIREFOX_DURATION_S, currentTimeS: 0, seeking: false, seeksIssued: 0 };
    assert.deepEqual(pageReturnStep(facts, { kind: "live" }), { kind: "hold" });
    assert.deepEqual(pageReturnStep({ ...facts, durationS: TWELVE_HOURS_S }, { kind: "live" }), { kind: "not_this_video" });
  });
});
