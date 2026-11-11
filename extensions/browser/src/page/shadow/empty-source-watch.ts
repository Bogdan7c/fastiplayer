// Сторож «элемент без источника» (Д-1, Д-8): пустой источник — пауза решения страницы, а не конец.
//
// Страница сняла источник (`srcObject = null`, `removeAttribute("src"); load()`, `src = ""`).
// Обычно следом ставится новый MediaSource (YouTube перед рекламой, hls.js/Shaka/dash.js при
// сбросе) — тогда это новое поколение, тень остаётся. Но страница могла и закрыть свой плеер, не
// убирая `<video>`: без сторожа тень держалась бы вечно. Поэтому ожидание ограничено допуском, а
// конец честный (решение 179: `element_released` → у плеера «The page took the video back»).
//
// Сессия 16a (решения 303, 306–308): допуск зависит от того, КОГДА снят источник.
// - Посреди серии — прежние 5 с (`EMPTY_SOURCE_GRACE_MS`), конец `element_released`.
// - После конца серии (часы плеера уже сказали `ended`) страница готовит следующую серию, а в скрытой
//   вкладке её таймеры спят до минуты: ждём `NEXT_EPISODE_WAIT_MS`, конец — отдельный
//   `next_episode_not_started` (фон закроет 4011 «The page didn't start the next episode»).
// Момент запоминается при ПЕРВОМ снятии и дальше не пересматривается: повторное снятие не продлевает
// и не меняет срок, иначе страница могла бы держать тень вечно.
//
// Как и `DetachedElementWatch`, сторож работает на тиках часов плеера (не таймером: таймеры
// скрытой вкладки Chromium будит раз в минуту). На каждом тике источник ПЕРЕКЛАССИФИЦИРУЕТСЯ:
// `setAttribute("src", …)` элементу в NETWORK_EMPTY не даёт `emptied`, и новый источник
// иначе прошёл бы мимо тени.

import { NEXT_EPISODE_WAIT_MS } from "../../shared/next-episode-wait.ts";
import type { SourceVerdict } from "./source-reset-policy.ts";

/**
 * Сколько элемент может стоять без источника посреди серии, прежде чем считать, что страница забрала
 * видео (решение владельца). Плееры ставят новый MediaSource за десятки миллисекунд; 5 с с запасом
 * покрывают и медленный запрос манифеста рекламы, а зритель за это время видит у нас последний
 * кадр, а не «ничего».
 */
export const EMPTY_SOURCE_GRACE_MS = 5_000;

/** Когда страница сняла источник: от этого зависит срок ожидания и причина конца. */
export type SourceRemovalMoment =
  /** Серия ещё шла: короткий допуск, потом «страница забрала видео». */
  | "mid_episode"
  /** Часы плеера уже сказали `ended`: страница готовит следующую серию, долгий допуск. */
  | "after_episode_end";

/** Что сделало снятие источника. */
export type SourceRemovalEffect =
  /** Ожидание уже шло: срок и момент прежние (отсчёт от первого снятия). */
  | "wait_continues"
  /** Начато короткое ожидание посреди серии. */
  | "mid_episode_wait_began"
  /** Начато ожидание следующей серии: об этом стоит сказать плееру (`awaiting_next_episode`). */
  | "next_episode_wait_began";

/** Что делать тени по итогам тика. */
export type EmptySourceTickDecision =
  | "keep_shadow"
  /** Страница забрала видео: чужой источник или пустота посреди серии дольше допуска. */
  | "release_to_page"
  /** Серия доиграла, а следующую страница не поставила за `NEXT_EPISODE_WAIT_MS`. */
  | "next_episode_not_started";

/** Идущее ожидание: с какого мгновения пусто и в какой момент серии снят источник. */
interface EmptySourceWait {
  readonly sinceMs: number;
  readonly moment: SourceRemovalMoment;
}

/** Срок ожидания для момента снятия источника. */
export function emptySourceLimitMs(moment: SourceRemovalMoment): number {
  return moment === "after_episode_end" ? NEXT_EPISODE_WAIT_MS : EMPTY_SOURCE_GRACE_MS;
}

export class EmptySourceWatch {
  /** Идущее ожидание (`null` — источник есть, сторож спит). */
  #wait: EmptySourceWait | null = null;

  /** Ждёт ли сторож нового источника. */
  get isWaiting(): boolean {
    return this.#wait !== null;
  }

  /**
   * Источник снят. Отсчёт и момент — от ПЕРВОГО снятия: повторное `srcObject = null` допуск не
   * продлевает и срок не меняет, иначе страница могла бы держать тень вечно.
   */
  sourceRemoved(nowMs: number, moment: SourceRemovalMoment): SourceRemovalEffect {
    if (this.#wait !== null) return "wait_continues";
    this.#wait = { sinceMs: nowMs, moment };
    return moment === "after_episode_end" ? "next_episode_wait_began" : "mid_episode_wait_began";
  }

  /** Поставлен новый MediaSource — ожидание кончилось. */
  mediaSourceArrived(): void {
    this.#wait = null;
  }

  /**
   * Тик часов. `readSource` зовётся только пока сторож ждёт (вне ожидания тик ничего не стоит).
   * Новый MediaSource снимает ожидание; чужой источник — конец сразу; пусто дольше срока — конец
   * с причиной по моменту снятия. Ровно на сроке — уже конец (`>=`).
   */
  onClockTick(readSource: () => SourceVerdict, nowMs: number): EmptySourceTickDecision {
    const wait = this.#wait;
    if (wait === null) return "keep_shadow";
    switch (readSource()) {
      case "media_source":
        this.#wait = null;
        return "keep_shadow";
      case "not_media_source":
        return "release_to_page";
      case "no_source":
        if (nowMs - wait.sinceMs < emptySourceLimitMs(wait.moment)) return "keep_shadow";
        return wait.moment === "after_episode_end" ? "next_episode_not_started" : "release_to_page";
    }
  }
}
