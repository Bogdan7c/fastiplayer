// Решение шага позднего входа (сессия 10d): что значит «та же секунда», реклама вместо фильма, отказ сайта.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  decideLateStep,
  isForeignVideo,
  LATE_FILM_WAIT_MS,
  LATE_SEEK_SETTLE_MS,
  NO_SEEK_YET,
  rememberedVideoOf,
  type LateStepFacts,
  type RememberedVideo,
} from "../../src/background/entries/late-entry-step.ts";
import type { LateVideoFact } from "../../src/background/entries/late-video-reader.ts";

const FILM: RememberedVideo = { position_s: 600, duration_s: 3_000 };
const fact = (position: number, duration: number | null, playing = true, area = 1000): LateVideoFact => ({ position_s: position, duration_s: duration, playing, visible_area_px: area });

function facts(overrides: Partial<LateStepFacts> = {}): LateStepFacts {
  return {
    remembered: FILM,
    mainDurationS: 3_000,
    mainPlaying: true,
    videos: [fact(0, 3_000)],
    seek: NO_SEEK_YET,
    nowMs: 10_000,
    foreignVideoPlayingSinceMs: null,
    ...overrides,
  };
}

const HAND_OFF_SAME = { kind: "hand_off", position: "same_second" };
const HAND_OFF_SITE = { kind: "hand_off", position: "site_position" };
const SEEK = { kind: "seek", target: { duration_s: 3_000, position_s: 600 } };

describe("какое видео запомнить", () => {
  it("играющее важнее большого", () => {
    assert.deepEqual(rememberedVideoOf([fact(50, 100, false, 9_000), fact(600, 3_000, true, 100)]), FILM);
  });

  it("среди равных берётся самое большое на экране", () => {
    assert.deepEqual(rememberedVideoOf([fact(50, 100, true, 10), fact(600, 3_000, true, 900)]), FILM);
  });

  it("эфир, начало ролика и пустая страница: запоминать нечего", () => {
    assert.equal(rememberedVideoOf([fact(600, null)]), null);
    assert.equal(rememberedVideoOf([fact(1, 3_000)]), null);
    assert.equal(rememberedVideoOf([]), null);
  });
});

describe("шаг позднего входа", () => {
  it("главного видео ещё нет: ждём", () => {
    assert.deepEqual(decideLateStep(facts({ mainDurationS: undefined })), { kind: "wait" });
  });

  it("запоминать было нечего: передаём первое заигравшее, на паузе ждём", () => {
    assert.deepEqual(decideLateStep(facts({ remembered: null })), HAND_OFF_SAME);
    assert.deepEqual(decideLateStep(facts({ remembered: null, mainPlaying: false })), { kind: "wait" });
  });

  it("то же видео не на секунде: ставим её", () => {
    assert.deepEqual(decideLateStep(facts()), SEEK);
  });

  it("видео на паузе тоже получает секунду: человек нажмёт play уже с неё", () => {
    assert.deepEqual(decideLateStep(facts({ mainPlaying: false, videos: [fact(0, 3_000, false)] })), SEEK);
  });

  it("сайт сам вернул на секунду: перематывать незачем, сразу передаём", () => {
    assert.deepEqual(decideLateStep(facts({ videos: [fact(601, 3_000)] })), HAND_OFF_SAME);
  });

  it("на секунде, но на паузе: ждём play", () => {
    assert.deepEqual(decideLateStep(facts({ mainPlaying: false, videos: [fact(600, 3_000, false)] })), { kind: "wait" });
  });

  it("после постановки допуск сверху растёт на проигранное время", () => {
    const seek = { attempts: 1, lastAttemptAtMs: 0 };
    assert.deepEqual(decideLateStep(facts({ seek, nowMs: 4_000, videos: [fact(605, 3_000)] })), HAND_OFF_SAME);
    assert.deepEqual(decideLateStep(facts({ seek, nowMs: 1_000, videos: [fact(605, 3_000)] })), { kind: "wait" }, "за секунду на 5 с не уедешь: это ещё не наша секунда");
  });

  it("допуск снизу: две секунды раньше — ещё та секунда, три — уже нет", () => {
    assert.deepEqual(decideLateStep(facts({ videos: [fact(598, 3_000)] })), HAND_OFF_SAME);
    assert.deepEqual(decideLateStep(facts({ videos: [fact(597, 3_000)] })), SEEK);
  });

  it("сайту даётся время исполнить перемотку", () => {
    const seek = { attempts: 1, lastAttemptAtMs: 10_000 };
    assert.deepEqual(decideLateStep(facts({ seek, nowMs: 10_000 + LATE_SEEK_SETTLE_MS - 1 })), { kind: "wait" });
  });

  it("первая попытка не прижилась: вторая (сайт сбросил позицию при запуске)", () => {
    const seek = { attempts: 1, lastAttemptAtMs: 0 };
    assert.deepEqual(decideLateStep(facts({ seek, nowMs: LATE_SEEK_SETTLE_MS })), SEEK);
  });

  it("сайт не дал встать и со второго раза: передаём откуда начал сайт и знаем об этом", () => {
    const seek = { attempts: 2, lastAttemptAtMs: 0 };
    assert.deepEqual(decideLateStep(facts({ seek, nowMs: LATE_SEEK_SETTLE_MS })), HAND_OFF_SITE);
  });

  it("на паузе срок перемотки не идёт: сайт мог отложить её до запуска", () => {
    const seek = { attempts: 2, lastAttemptAtMs: 0 };
    assert.deepEqual(decideLateStep(facts({ seek, nowMs: LATE_SEEK_SETTLE_MS * 10, mainPlaying: false })), { kind: "wait" });
  });

  it("реклама с другой длительностью: секунду ей не ставим, ждём фильм", () => {
    const step = decideLateStep(facts({ mainDurationS: 30, videos: [fact(1, 30)], foreignVideoPlayingSinceMs: 9_000 }));
    assert.deepEqual(step, { kind: "wait" });
  });

  it("фильм так и не вышел за срок: отдаём что играет и говорим, что секунда не сохранена", () => {
    const step = decideLateStep(facts({ mainDurationS: 30, videos: [fact(1, 30)], foreignVideoPlayingSinceMs: 0, nowMs: LATE_FILM_WAIT_MS }));
    assert.deepEqual(step, HAND_OFF_SITE);
  });

  it("ровно на пороге длительность ещё своя, за ним уже чужая", () => {
    assert.equal(isForeignVideo(3_002, FILM), false);
    assert.equal(isForeignVideo(3_003, FILM), true);
    assert.equal(isForeignVideo(null, FILM), true, "эфир вместо запомненного ролика");
  });

  it("опрос видит видео, а чтение DOM его не нашло: поставить нечем, передаём как есть", () => {
    assert.deepEqual(decideLateStep(facts({ videos: [] })), HAND_OFF_SITE);
  });
});
