// Пульс «тень жива» (решение 180, Д-2): на тиках часов, не чаще интервала, только пока тень жива.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createShadowHarness, playingVideo, pulseCount } from "./shadow-harness.ts";
import { EMPTY_SOURCE_GRACE_MS } from "../../../src/page/shadow/empty-source-watch.ts";
import { SHADOW_PULSE_INTERVAL_MS } from "../../../src/shared/handoff-contract.ts";

describe("тень: пульс «тень жива» (решение 180, Д-2)", () => {
  it("раз в SHADOW_PULSE_INTERVAL_MS на тиках часов, первый — на первом тике", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    assert.equal(pulseCount(harness), 0, "без тиков пульса нет");
    harness.tick(session, 10, "playing");
    assert.equal(pulseCount(harness), 1, "первый тик — первый пульс");
    // Тики каждые 250 мс: пульс только когда с прошлого прошло ≥ интервала.
    for (let tick = 1; tick <= 12; tick += 1) {
      harness.clock.advance(250);
      harness.tick(session, 10 + tick * 0.25, "paused");
    }
    assert.equal(pulseCount(harness), 1 + Math.floor((12 * 250) / SHADOW_PULSE_INTERVAL_MS));
  });

  it("без тиков пульса нет, сколько бы времени ни прошло и что бы ни делала страница", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    harness.clock.advance(60_000);
    video.pause();
    video.currentTime = 5;
    assert.equal(pulseCount(harness), 0);
  });

  it("после конца тени (возврат или страница забрала элемент) пульса нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    const pulsesAtEnd = pulseCount(harness);
    for (let tick = 0; tick < 10; tick += 1) {
      harness.clock.advance(SHADOW_PULSE_INTERVAL_MS);
      harness.tick(session, 10, "playing");
    }
    assert.equal(pulseCount(harness), pulsesAtEnd);
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "returned" });
  });

  it("тик, на котором тень снялась (элемент без источника дольше допуска), пульс не даёт", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.removeAttribute("src");
    video.srcObject = null;
    harness.clock.advance(EMPTY_SOURCE_GRACE_MS + SHADOW_PULSE_INTERVAL_MS);
    harness.tick(session, 30, "playing");
    harness.clock.advance(EMPTY_SOURCE_GRACE_MS);
    harness.tick(session, 30, "playing");
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "element_released" });
  });
});
