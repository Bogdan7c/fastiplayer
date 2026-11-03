// Сторож «элемент без источника» (Д-1, Д-8): пустой источник — пауза решения страницы, а не конец.
//
// Страница сняла источник (`srcObject = null`, `removeAttribute("src"); load()`, `src = ""`).
// Обычно следом ставится новый MediaSource (YouTube перед рекламой, hls.js/Shaka/dash.js при
// сбросе) — тогда это новое поколение, тень остаётся. Но страница могла и закрыть свой плеер, не
// убирая `<video>`: без сторожа тень держалась бы вечно. Поэтому ожидание ограничено допуском, а
// конец честный (решение 179: `element_released` → у плеера «The page took the video back»).
//
// Как и `DetachedElementWatch`, сторож работает на тиках часов плеера (не таймером: таймеры
// скрытой вкладки Chromium будит раз в минуту). На каждом тике источник ПЕРЕКЛАССИФИЦИРУЕТСЯ:
// `setAttribute("src", …)` элементу в NETWORK_EMPTY не даёт `emptied`, и новый источник
// иначе прошёл бы мимо тени.

import type { SourceVerdict } from "./source-reset-policy.ts";

/**
 * Сколько элемент может стоять без источника, прежде чем считать, что страница забрала видео
 * (решение владельца). Плееры ставят новый MediaSource за десятки миллисекунд; 5 с с запасом
 * покрывают и медленный запрос манифеста рекламы, а зритель за это время видит у нас последний
 * кадр, а не «ничего».
 */
export const EMPTY_SOURCE_GRACE_MS = 5_000;

/** Что делать тени по итогам тика. */
export type EmptySourceTickDecision = "keep_shadow" | "release_to_page";

export class EmptySourceWatch {
  /** Когда источник пропал (`null` — источник есть, сторож спит). */
  #emptySinceMs: number | null = null;

  /** Ждёт ли сторож нового источника. */
  get isWaiting(): boolean {
    return this.#emptySinceMs !== null;
  }

  /**
   * Источник снят. Отсчёт идёт от ПЕРВОГО снятия: повторное `srcObject = null` допуск не
   * продлевает, иначе страница могла бы держать тень вечно.
   */
  sourceRemoved(nowMs: number): void {
    if (this.#emptySinceMs === null) this.#emptySinceMs = nowMs;
  }

  /** Поставлен новый MediaSource — ожидание кончилось. */
  mediaSourceArrived(): void {
    this.#emptySinceMs = null;
  }

  /**
   * Тик часов. `readSource` зовётся только пока сторож ждёт (вне ожидания тик ничего не стоит).
   * Новый MediaSource снимает ожидание; чужой источник — конец сразу; пусто дольше допуска — конец.
   */
  onClockTick(readSource: () => SourceVerdict, nowMs: number): EmptySourceTickDecision {
    const emptySinceMs = this.#emptySinceMs;
    if (emptySinceMs === null) return "keep_shadow";
    switch (readSource()) {
      case "media_source":
        this.#emptySinceMs = null;
        return "keep_shadow";
      case "not_media_source":
        return "release_to_page";
      case "no_source":
        return nowMs - emptySinceMs >= EMPTY_SOURCE_GRACE_MS ? "release_to_page" : "keep_shadow";
    }
  }
}
