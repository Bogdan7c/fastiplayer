// Сводка «реклама на экране» (09d): все входы, дубли, молчание фрейма, приоритеты.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { AD_REPORT_SILENCE_MS, createAdOnScreenSummary } from "../../src/background/handoff/ad-on-screen.ts";
import type { PageAdState } from "../../src/protocol/index.ts";
import { createFakeClock } from "./support.ts";

const NONE: PageAdState = { kind: "none" };
const NO_ORGANS: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "unavailable" } };
const AVAILABLE: PageAdState = { kind: "showing", pod: { kind: "known", index: 1, count: 2 }, skip: { kind: "available" } };
const ON_PAGE: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "on_page" } };
const COUNTDOWN: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "countdown", seconds: 4 } };

function setup() {
  const clock = createFakeClock();
  const announced: PageAdState[] = [];
  let sessionSaysAd = false;
  const summary = createAdOnScreenSummary({
    now: () => clock.scheduler.now(),
    sessionSaysAdOnScreen: () => sessionSaysAd,
    onChange: (ad) => void announced.push(ad),
  });
  return { clock, announced, summary, setSessionSaysAd: (value: boolean) => void (sessionSaysAd = value) };
}

describe("сводка: правило", () => {
  it("ничего не известно — none, и это не объявляется (плеер и так не знает о рекламе)", () => {
    const world = setup();
    world.summary.recompute();
    assert.deepEqual(world.summary.current(), NONE);
    assert.deepEqual(world.announced, []);
  });

  it("только признак сеанса — «реклама есть, органов не знаем»", () => {
    const world = setup();
    world.setSessionSaysAd(true);
    world.summary.recompute();
    assert.deepEqual(world.announced, [NO_ORGANS]);
  });

  it("донесение фрейма сильнее признака сеанса", () => {
    const world = setup();
    world.setSessionSaysAd(true);
    world.summary.recompute();
    world.summary.reportFromFrame(3, AVAILABLE);
    assert.deepEqual(world.announced, [NO_ORGANS, AVAILABLE]);
    assert.deepEqual(world.summary.current(), AVAILABLE);
  });

  it("донесение none снимает прежнее донесение фрейма; остаётся признак сеанса, а без него — none", () => {
    const world = setup();
    world.setSessionSaysAd(true);
    world.summary.reportFromFrame(3, AVAILABLE);
    world.summary.reportFromFrame(3, NONE);
    assert.deepEqual(world.summary.current(), NO_ORGANS);
    world.setSessionSaysAd(false);
    world.summary.recompute();
    assert.deepEqual(world.announced, [AVAILABLE, NO_ORGANS, NONE]);
  });

  it("признак сеанса меняется при переезде — сводка пересчитывается по recompute", () => {
    const world = setup();
    world.setSessionSaysAd(true);
    world.summary.recompute();
    world.setSessionSaysAd(false);
    world.summary.recompute();
    world.setSessionSaysAd(true);
    world.summary.recompute();
    assert.deepEqual(world.announced, [NO_ORGANS, NONE, NO_ORGANS]);
  });

  it("повторы не уходят: одно и то же донесение, пересчёт без перемен", () => {
    const world = setup();
    for (let repeat = 0; repeat < 5; repeat += 1) {
      world.summary.reportFromFrame(1, ON_PAGE);
      world.summary.recompute();
    }
    assert.deepEqual(world.announced, [ON_PAGE]);
  });

  it("несколько фреймов: нажимаемая кнопка полезнее «нажмёт человек», та полезнее отсчёта, при равенстве меньший номер", () => {
    const world = setup();
    world.summary.reportFromFrame(9, COUNTDOWN);
    world.summary.reportFromFrame(8, ON_PAGE);
    assert.deepEqual(world.summary.current(), ON_PAGE);
    world.summary.reportFromFrame(7, AVAILABLE);
    assert.deepEqual(world.summary.current(), AVAILABLE);
    assert.equal(world.summary.frameWithAvailableSkip(), 7);
    world.summary.reportFromFrame(2, { ...AVAILABLE, pod: { kind: "unknown" } });
    assert.equal(world.summary.frameWithAvailableSkip(), 7, "известный номер блока полезнее при той же кнопке");
    world.summary.reportFromFrame(7, NONE);
    assert.equal(world.summary.frameWithAvailableSkip(), 2);
  });

  it("frameWithAvailableSkip: только фрейм, сказавший available; on_page и отсчёт — никто", () => {
    const world = setup();
    assert.equal(world.summary.frameWithAvailableSkip(), undefined);
    world.summary.reportFromFrame(4, ON_PAGE);
    world.summary.reportFromFrame(5, COUNTDOWN);
    assert.equal(world.summary.frameWithAvailableSkip(), undefined);
  });
});

describe("сводка: молчание фрейма — конец рекламы", () => {
  it("IMA-фрейм удалён и молчит: донесение живёт AD_REPORT_SILENCE_MS и истекает", () => {
    const world = setup();
    world.summary.reportFromFrame(2, AVAILABLE);
    world.clock.advance(AD_REPORT_SILENCE_MS - 1);
    world.summary.recompute();
    assert.deepEqual(world.summary.current(), AVAILABLE);
    assert.equal(world.summary.frameWithAvailableSkip(), 2);
    world.clock.advance(1);
    world.summary.recompute();
    assert.deepEqual(world.announced, [AVAILABLE, NONE]);
    assert.equal(world.summary.frameWithAvailableSkip(), undefined);
  });

  it("повторное донесение продлевает жизнь", () => {
    const world = setup();
    for (let second = 0; second < 10; second += 1) {
      world.summary.reportFromFrame(2, AVAILABLE);
      world.clock.advance(1_000);
      world.summary.recompute();
    }
    assert.deepEqual(world.announced, [AVAILABLE]);
  });

  it("донесение истекло, а признак сеанса держится — остаётся «реклама есть, органов не знаем»", () => {
    const world = setup();
    world.setSessionSaysAd(true);
    world.summary.reportFromFrame(2, AVAILABLE);
    world.clock.advance(AD_REPORT_SILENCE_MS);
    world.summary.recompute();
    assert.deepEqual(world.announced, [AVAILABLE, NO_ORGANS]);
  });
});
