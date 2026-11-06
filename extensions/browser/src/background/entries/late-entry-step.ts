// Решение одного шага позднего входа (сессия 10d). Чистая функция: все правила здесь и проверяются без
// браузера, а `late-entry.ts` только добывает факты и исполняет решение.
//
// Что значит «та же секунда». До перезагрузки запомнили позицию и длительность видео. После неё:
//  - видео с другой длительностью — не наше: скорее всего реклама. Секунду ему не ставим и ждём фильм
//    (выбор владельца: реклама доигрывает в браузере, фильм уходит в плеер с той же секунды);
//  - видео с той же длительностью стоит не там — ставим ему секунду и даём сайту время подчиниться;
//  - видео на месте и играет — передаём;
//  - сайт так и не встал на секунду (две попытки) — передаём как есть и честно говорим об этом.

import { LATE_POSITION_TOLERANCE_S, type LateSeekTarget, type LateVideoFact } from "./late-video-reader.ts";

/**
 * Сколько сайт может держать видео с другой длительностью, прежде чем отдадим что играет. Минута:
 * обычная реклама укладывается, а вечное ожидание хуже потерянной секунды.
 */
export const LATE_FILM_WAIT_MS = 60_000;

/** Сколько даём сайту исполнить перемотку и доехать до секунды (подгрузка куска, сеть). */
export const LATE_SEEK_SETTLE_MS = 5_000;

/** Сколько раз ставим секунду. Второй раз нужен сайтам, сбросившим позицию при первом запуске. */
export const LATE_SEEK_ATTEMPTS = 2;

/** Младше этой секунды запоминать нечего: сайт сам начнёт почти там же. */
export const LATE_MIN_REMEMBERED_POSITION_S = LATE_POSITION_TOLERANCE_S;

/** Видео, на котором стоял человек до перезагрузки. */
export interface RememberedVideo {
  readonly position_s: number;
  readonly duration_s: number;
}

/** Что сделано для постановки на секунду. */
export interface LateSeekProgress {
  readonly attempts: number;
  /** Часы фона (мс) последней попытки; `null` — попыток не было. */
  readonly lastAttemptAtMs: number | null;
}

export const NO_SEEK_YET: LateSeekProgress = { attempts: 0, lastAttemptAtMs: null };

/** Факты шага. Главное видео уже из НОВОГО документа: старый опрос сюда не доходит. */
export interface LateStepFacts {
  readonly remembered: RememberedVideo | null;
  /** Длительность главного видео; `undefined` — главного пока нет; `null` — эфир или неизвестна. */
  readonly mainDurationS: number | null | undefined;
  readonly mainPlaying: boolean;
  /** Все видео страницы (из них видно позицию нужного). */
  readonly videos: readonly LateVideoFact[];
  readonly seek: LateSeekProgress;
  readonly nowMs: number;
  /** Когда заиграло видео с другой длительностью; `null` — такого не было. */
  readonly foreignVideoPlayingSinceMs: number | null;
}

/** Откуда видео начнёт играть в плеере: это решает, что сказать человеку. */
export type LatePositionResult = "same_second" | "site_position";

export type LateStep =
  | { readonly kind: "wait" }
  | { readonly kind: "seek"; readonly target: LateSeekTarget }
  | { readonly kind: "hand_off"; readonly position: LatePositionResult };

const WAIT: LateStep = { kind: "wait" };

/** Какое видео запомнить: то, что играет, среди равных самое большое на экране; с позицией и длиной. */
export function rememberedVideoOf(videos: readonly LateVideoFact[]): RememberedVideo | null {
  let best: LateVideoFact | undefined;
  for (const video of videos) {
    if (best === undefined || isMoreLikelyWatched(video, best)) best = video;
  }
  if (best === undefined || best.duration_s === null || best.position_s < LATE_MIN_REMEMBERED_POSITION_S) return null;
  return { position_s: best.position_s, duration_s: best.duration_s };
}

function isMoreLikelyWatched(video: LateVideoFact, incumbent: LateVideoFact): boolean {
  if (video.playing !== incumbent.playing) return video.playing;
  return video.visible_area_px > incumbent.visible_area_px;
}

function sameDuration(left: number, right: number): boolean {
  return Math.abs(left - right) <= LATE_POSITION_TOLERANCE_S;
}

/** Видео не то, на котором стоял человек: другая длительность или эфир. Обычно это реклама перед фильмом. */
export function isForeignVideo(durationS: number | null, remembered: RememberedVideo): boolean {
  return durationS === null || !sameDuration(durationS, remembered.duration_s);
}

/** Видео стоит на нужной секунде: снизу допуск, сверху ещё и время, что оно успело проиграть с попытки. */
function isAtRememberedSecond(positionS: number, remembered: RememberedVideo, seek: LateSeekProgress, nowMs: number): boolean {
  const playedSinceSeekS = seek.lastAttemptAtMs === null ? 0 : (nowMs - seek.lastAttemptAtMs) / 1_000;
  return positionS >= remembered.position_s - LATE_POSITION_TOLERANCE_S && positionS <= remembered.position_s + LATE_POSITION_TOLERANCE_S + playedSinceSeekS;
}

export function decideLateStep(facts: LateStepFacts): LateStep {
  const { remembered, mainDurationS, mainPlaying, seek, nowMs } = facts;
  if (mainDurationS === undefined) return WAIT;
  // Запоминать было нечего (эфир, самое начало, видео не нашлось): передаём первое, что заиграло.
  if (remembered === null) return mainPlaying ? { kind: "hand_off", position: "same_second" } : WAIT;
  // Эфир вместо запомненного ролика или реклама: секунда к нему не относится, ждём фильм ограниченное время.
  if (isForeignVideo(mainDurationS, remembered)) {
    const waitedEnough = facts.foreignVideoPlayingSinceMs !== null && nowMs - facts.foreignVideoPlayingSinceMs >= LATE_FILM_WAIT_MS;
    return mainPlaying && waitedEnough ? { kind: "hand_off", position: "site_position" } : WAIT;
  }
  const film = facts.videos.find((video) => video.duration_s !== null && sameDuration(video.duration_s, remembered.duration_s));
  // Опрос видит видео, а чтение DOM нет (редкая разметка): поставить секунду нечем, передаём как есть.
  if (film === undefined) return mainPlaying ? { kind: "hand_off", position: "site_position" } : WAIT;
  if (isAtRememberedSecond(film.position_s, remembered, seek, nowMs)) return mainPlaying ? { kind: "hand_off", position: "same_second" } : WAIT;
  return decideSeek(facts, remembered);
}

/** Видео не на секунде: поставить, подождать или признать, что сайт не даёт. */
function decideSeek(facts: LateStepFacts, remembered: RememberedVideo): LateStep {
  const { seek, nowMs, mainPlaying } = facts;
  const target: LateStep = { kind: "seek", target: { duration_s: remembered.duration_s, position_s: remembered.position_s } };
  if (seek.lastAttemptAtMs === null) return target;
  // На паузе сайт перемотку часто откладывает до запуска: срок идёт, только пока видео играет.
  if (!mainPlaying || nowMs - seek.lastAttemptAtMs < LATE_SEEK_SETTLE_MS) return WAIT;
  return seek.attempts < LATE_SEEK_ATTEMPTS ? target : { kind: "hand_off", position: "site_position" };
}
