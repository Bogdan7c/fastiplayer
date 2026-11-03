// Модель часов тени на чистых числах: позиция, события, перемотки, конец.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  MAX_EXTRAPOLATION_MS,
  MAX_UNCONFIRMED_TICKS,
  READY_STATE,
  SOURCE_RESET_GIVE_UP_MS,
  ShadowTimeline,
  type TimelineSignal,
} from "../../../src/page/shadow/shadow-timeline.ts";

const SECOND_US = 1_000_000;

function createTimeline(options: { paused?: boolean; positionUs?: number; durationUs?: number | undefined } = {}) {
  const signals: TimelineSignal[] = [];
  const durationUs = "durationUs" in options ? options.durationUs : 100 * SECOND_US;
  const timeline = new ShadowTimeline(
    {
      positionUs: options.positionUs ?? 10 * SECOND_US,
      paused: options.paused ?? false,
      pageRate: 1,
      readyState: READY_STATE.HAVE_ENOUGH_DATA,
      nowMs: 0,
    },
    () => durationUs,
    (signal) => void signals.push(signal),
  );
  const eventTypes = () => signals.filter((signal) => signal.kind === "event").map((signal) => (signal as { type: string }).type);
  return { timeline, signals, eventTypes, clear: () => void (signals.length = 0) };
}

const clock = (positionUs: number, state: "playing" | "paused" | "waiting" | "ended", rate = 1000) => ({ position_us: positionUs, state, rate });

describe("модель тени: часы", () => {
  it("между тиками позиция интерполируется со скоростью часов и замирает после MAX_EXTRAPOLATION_MS", () => {
    const { timeline } = createTimeline();
    timeline.onClock(clock(20 * SECOND_US, "playing", 2000), 1_000);
    assert.equal(timeline.positionUs(1_000), 20 * SECOND_US);
    // 100 мс на скорости 2× = 200 мс медиавремени.
    assert.equal(timeline.positionUs(1_100), 20 * SECOND_US + 200_000);
    // Дальше предела интерполяции тень не убегает: правда — часы.
    assert.equal(timeline.positionUs(1_000 + MAX_EXTRAPOLATION_MS + 5_000), 20 * SECOND_US + MAX_EXTRAPOLATION_MS * 2000);
  });

  it("на паузе страницы и при waiting позиция не идёт", () => {
    const { timeline } = createTimeline();
    timeline.onClock(clock(20 * SECOND_US, "waiting"), 0);
    assert.equal(timeline.positionUs(500), 20 * SECOND_US);
    timeline.onClock(clock(20 * SECOND_US, "playing"), 500);
    timeline.pause(600);
    assert.equal(timeline.positionUs(5_000), 20 * SECOND_US + 100_000);
  });

  it("timeupdate на тиках игры — не чаще раза в 250 мс, кадры — на каждом тике игры", () => {
    const { timeline, signals, eventTypes } = createTimeline();
    timeline.onClock(clock(10 * SECOND_US, "playing"), 0);
    signals.length = 0;
    // Плеер тикает каждые 100 мс в течение секунды.
    for (let tick = 1; tick <= 10; tick += 1) timeline.onClock(clock(10 * SECOND_US + tick * 100_000, "playing"), tick * 100);
    // Первый timeupdate был на тике 0 мс (он стёрт), дальше — 300, 600, 900 мс.
    assert.equal(eventTypes().filter((type) => type === "timeupdate").length, 3);
    assert.equal(signals.filter((signal) => signal.kind === "frame_presented").length, 10);
  });

  it("на паузе тики не шлют timeupdate", () => {
    const { timeline, eventTypes, clear } = createTimeline({ paused: true });
    clear();
    for (let tick = 1; tick <= 8; tick += 1) timeline.onClock(clock(10 * SECOND_US, "paused"), tick * 250);
    assert.deepEqual(eventTypes(), []);
  });

  it("waiting → readyState падает, timeupdate+waiting; снова playing → canplay, playing, canplaythrough", () => {
    const { timeline, signals, eventTypes, clear } = createTimeline();
    timeline.onClock(clock(10 * SECOND_US, "playing"), 0);
    clear();
    timeline.onClock(clock(11 * SECOND_US, "waiting"), 1_000);
    assert.equal(timeline.readyState, READY_STATE.HAVE_CURRENT_DATA);
    assert.deepEqual(eventTypes(), ["timeupdate", "waiting"]);
    clear();
    timeline.onClock(clock(11 * SECOND_US, "playing"), 2_000);
    assert.equal(timeline.readyState, READY_STATE.HAVE_ENOUGH_DATA);
    assert.deepEqual(eventTypes(), ["canplay", "playing", "canplaythrough", "timeupdate"]);
    assert.ok(signals.some((signal) => signal.kind === "settle_play_promises" && signal.outcome === "resolve"));
  });

  it("ended → timeupdate, pause, ended; повтор ended событий не шлёт", () => {
    const { timeline, eventTypes, clear } = createTimeline();
    timeline.onClock(clock(99 * SECOND_US, "playing"), 0);
    clear();
    timeline.onClock(clock(100 * SECOND_US, "ended"), 1_000);
    assert.deepEqual(eventTypes(), ["timeupdate", "pause", "ended"]);
    assert.equal(timeline.ended, true);
    assert.equal(timeline.paused, true);
    clear();
    timeline.onClock(clock(100 * SECOND_US, "ended"), 1_250);
    assert.deepEqual(eventTypes(), []);
  });

  it("шаг назад по часам без перемотки (новое поколение) — не перемотка: без seeking, позиция принята", () => {
    const { timeline, eventTypes, clear } = createTimeline();
    timeline.onClock(clock(30 * SECOND_US, "playing"), 0);
    clear();
    timeline.onClock(clock(0, "playing"), 250);
    assert.equal(timeline.positionUs(250), 0);
    assert.equal(timeline.seeking, false);
    assert.ok(!eventTypes().includes("seeking"));
  });

  it("мелкое дрожание назад не видно странице: позиция не убывает", () => {
    const { timeline } = createTimeline();
    timeline.onClock(clock(10 * SECOND_US, "playing"), 0);
    const shown = timeline.positionUs(240); // 10,24 с
    timeline.onClock(clock(10 * SECOND_US + 200_000, "playing"), 250); // тик «позади» на 40 мс
    assert.equal(timeline.positionUs(250), shown);
  });
});

describe("модель тени: перемотка", () => {
  it("перемотка плеером: цель сразу, seeking и HAVE_METADATA; seeked — на следующем тике", () => {
    const { timeline, eventTypes, clear } = createTimeline();
    timeline.onClock(clock(10 * SECOND_US, "playing"), 0);
    clear();
    const target = timeline.seek("player_seek", 50 * SECOND_US, 100);
    assert.equal(target, 50 * SECOND_US);
    assert.equal(timeline.positionUs(5_000), 50 * SECOND_US);
    assert.equal(timeline.seeking, true);
    assert.equal(timeline.readyState, READY_STATE.HAVE_METADATA);
    assert.deepEqual(eventTypes(), ["seeking", "timeupdate", "waiting"]);
    clear();
    timeline.onClock(clock(50 * SECOND_US + 10_000, "playing"), 300);
    assert.equal(timeline.seeking, false);
    assert.equal(timeline.positionUs(300), 50 * SECOND_US + 10_000);
    assert.deepEqual(eventTypes().slice(0, 2), ["timeupdate", "seeked"]);
    assert.ok(eventTypes().includes("playing"));
  });

  it("перемотка за конец: цель = длительность и после подтверждения — ended", () => {
    const { timeline, eventTypes, clear } = createTimeline();
    timeline.onClock(clock(10 * SECOND_US, "playing"), 0);
    const target = timeline.seek("player_seek", 500 * SECOND_US, 100);
    assert.equal(target, 100 * SECOND_US);
    clear();
    timeline.onClock(clock(100 * SECOND_US, "ended"), 300);
    assert.equal(timeline.ended, true);
    assert.equal(timeline.paused, true);
    assert.equal(timeline.positionUs(300), 100 * SECOND_US);
    assert.deepEqual(eventTypes().slice(0, 4), ["timeupdate", "seeked", "pause", "ended"]);
  });

  it("перемотка ограничена нулём снизу и не ограничена сверху без длительности", () => {
    const { timeline } = createTimeline({ durationUs: undefined });
    assert.equal(timeline.seek("page_seek", -5 * SECOND_US, 0), 0);
    assert.equal(timeline.seek("page_seek", 5_000 * SECOND_US, 0), 5_000 * SECOND_US);
    assert.equal(timeline.ended, false);
  });

  it("перемотка страницей: старые тики не отматывают позицию, близкий тик подтверждает", () => {
    const { timeline, eventTypes, clear } = createTimeline();
    timeline.onClock(clock(10 * SECOND_US, "playing"), 0);
    timeline.seek("page_seek", 60 * SECOND_US, 100);
    clear();
    // Тик, отправленный плеером до того, как он узнал о перемотке.
    timeline.onClock(clock(10 * SECOND_US + 250_000, "playing"), 250);
    assert.equal(timeline.positionUs(250), 60 * SECOND_US);
    assert.equal(timeline.seeking, true);
    assert.deepEqual(eventTypes(), []);
    timeline.onClock(clock(60 * SECOND_US + 100_000, "playing"), 500);
    assert.equal(timeline.seeking, false);
    assert.equal(timeline.positionUs(500), 60 * SECOND_US + 100_000);
  });

  it("перемотка страницей, которую плеер поставил не туда: принимается после MAX_UNCONFIRMED_TICKS тиков", () => {
    const { timeline } = createTimeline();
    timeline.seek("page_seek", 60 * SECOND_US, 0);
    for (let tick = 1; tick <= MAX_UNCONFIRMED_TICKS; tick += 1) {
      timeline.onClock(clock(30 * SECOND_US, "playing"), tick * 250);
      assert.equal(timeline.seeking, true, `тик ${tick}`);
    }
    timeline.onClock(clock(30 * SECOND_US, "playing"), (MAX_UNCONFIRMED_TICKS + 1) * 250);
    assert.equal(timeline.seeking, false);
    assert.equal(timeline.positionUs((MAX_UNCONFIRMED_TICKS + 1) * 250), 30 * SECOND_US);
  });

  it("play() страницы после конца ролика перематывает на начало", () => {
    const { timeline } = createTimeline();
    timeline.onClock(clock(100 * SECOND_US, "ended"), 0);
    const result = timeline.play("page", 100);
    assert.equal(result.restartedAtUs, 0);
    assert.equal(result.unpaused, true);
    assert.equal(timeline.ended, false);
    assert.equal(timeline.positionUs(100), 0);
  });
});

describe("модель тени: новый источник (Д-4)", () => {
  /** Тики старого поколения: плеер ещё показывает прошлый фильм на 2953 с. */
  const OLD_GENERATION_US = 2953 * SECOND_US;

  it("после resetForNewSource 20 тиков старой позиции (играет, затем ended) не сдвигают тень и не дают ended", () => {
    const { timeline, eventTypes, clear } = createTimeline({ durationUs: undefined });
    timeline.onClock(clock(OLD_GENERATION_US, "playing"), 0);
    timeline.resetForNewSource(1, 100);
    // Страница сразу запускает новое видео (рекламу): ждёт его данных.
    timeline.play("page", 100);
    clear();
    let nowMs = 100;
    for (let tick = 0; tick < 20; tick += 1) {
      nowMs += 250;
      timeline.onClock(clock(OLD_GENERATION_US + tick * 250_000, "playing"), nowMs);
    }
    // Старое поколение доиграло до отсечки и отчиталось концом.
    for (let tick = 0; tick < 20; tick += 1) {
      nowMs += 250;
      timeline.onClock(clock(OLD_GENERATION_US + 5 * SECOND_US, "ended"), nowMs);
    }
    assert.equal(timeline.positionUs(nowMs), 0);
    assert.equal(timeline.ended, false);
    assert.equal(timeline.paused, false, "play() страницы не отменён чужим концом");
    assert.ok(!eventTypes().includes("ended"), JSON.stringify(eventTypes()));
    assert.ok(!eventTypes().includes("pause"), JSON.stringify(eventTypes()));
  });

  it("плеер признал новый источник тиком waiting: следующий тик принят как есть (эфир с 3600 с)", () => {
    const { timeline } = createTimeline({ durationUs: undefined });
    timeline.onClock(clock(OLD_GENERATION_US, "playing"), 0);
    timeline.resetForNewSource(1, 100);
    timeline.onClock(clock(OLD_GENERATION_US, "playing"), 350);
    assert.equal(timeline.positionUs(350), 0);
    timeline.onClock(clock(3600 * SECOND_US, "waiting"), 600);
    assert.equal(timeline.positionUs(600), 0, "сам тик ожидания далеко от цели не подтверждает");
    timeline.onClock(clock(3600 * SECOND_US, "playing"), 850);
    assert.equal(timeline.positionUs(850), 3600 * SECOND_US);
  });

  it("тик у цели (±1 с) подтверждает новый источник сразу", () => {
    const { timeline } = createTimeline({ durationUs: undefined });
    timeline.resetForNewSource(1, 0);
    timeline.onClock(clock(OLD_GENERATION_US, "playing"), 250);
    timeline.onClock(clock(300_000, "playing"), 500);
    assert.equal(timeline.positionUs(500), 300_000);
  });

  it("страховка по времени: без признания плеера чужой тик принимается не раньше SOURCE_RESET_GIVE_UP_MS и никогда — ended", () => {
    assert.ok(SOURCE_RESET_GIVE_UP_MS >= 10_000);
    const { timeline, eventTypes } = createTimeline({ durationUs: undefined });
    timeline.resetForNewSource(1, 0);
    timeline.onClock(clock(OLD_GENERATION_US, "playing"), SOURCE_RESET_GIVE_UP_MS - 1);
    assert.equal(timeline.positionUs(SOURCE_RESET_GIVE_UP_MS - 1), 0);
    timeline.onClock(clock(OLD_GENERATION_US, "ended"), SOURCE_RESET_GIVE_UP_MS + 250);
    assert.equal(timeline.positionUs(SOURCE_RESET_GIVE_UP_MS + 250), 0);
    assert.equal(timeline.ended, false);
    assert.ok(!eventTypes().includes("ended"));
    timeline.onClock(clock(OLD_GENERATION_US, "paused"), SOURCE_RESET_GIVE_UP_MS + 500);
    assert.equal(timeline.positionUs(SOURCE_RESET_GIVE_UP_MS + 500), OLD_GENERATION_US);
  });
});
