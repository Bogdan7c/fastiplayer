// Шаг сторожа при взводе возврата страницы (сессия 11b2, решения 240–241): чистое решение по фактам элемента.
//
// Вкладку открыли заново, и сторож ждёт то самое видео, что играет в окне плеера. Порядок для записи:
// 1. метаданных нет — ждать (длину ещё не сверить);
// 2. длина не та (реклама, превью) — видео не наше: пусть играет в браузере, вкладка без звука;
// 3. страница не на секунде плеера — сначала ЧЕСТНАЯ перемотка самой страницы (настоящий `currentTime`,
//    до тени): плеер сайта видит обычную перемотку и сам качает данные с этой секунды;
// 4. перемотка ещё идёт — ждать `seeked`;
// 5. страница на месте — ставить тень.
// Эфир длины не имеет: видео с бесконечной длиной берётся сразу, страница сама входит на живой край.
//
// Перемотка не повторяется бесконечно: сайт может сам сбросить позицию (свой «продолжить с места»). После
// `MAX_RETURN_SEEKS` попыток тень встаёт там, где страница, — плеер увидит это в `page_seek` и встанет так же.

import { isEndlessDurationSeconds } from "../shared/endless-duration.ts";
import { PAGE_RETURN_DURATION_TOLERANCE_S, type PageReturnTarget } from "../shared/handoff-contract.ts";

/** `HTMLMediaElement.HAVE_METADATA`: длительность и размер кадра известны. */
const HAVE_METADATA = 1;

/** Насколько страница может стоять мимо секунды плеера, чтобы считаться «на месте», секунды. */
export const RETURN_POSITION_SETTLE_S = 1;

/** Сколько раз сторож сам перематывает страницу на секунду плеера, прежде чем принять её позицию. */
export const MAX_RETURN_SEEKS = 2;

/** Что сторож знает об элементе в момент решения. */
export interface ReturnVideoFacts {
  readonly readyState: number;
  /** `duration` элемента: `NaN` — неизвестна, `Infinity` и длиннее года — эфир. */
  readonly durationS: number;
  readonly currentTimeS: number;
  /** Настоящий элемент сейчас перематывается. */
  readonly seeking: boolean;
  /** Сколько раз сторож уже перематывал этот элемент. */
  readonly seeksIssued: number;
}

/** Решение сторожа. */
export type PageReturnStep =
  /** Пока решать рано: нет метаданных или идёт перемотка. */
  | { readonly kind: "wait" }
  /** Видео не то, что в плеере: его не трогаем, страница играет его сама. */
  | { readonly kind: "not_this_video" }
  /** Сначала перемотать страницу на секунду плеера. */
  | { readonly kind: "seek_to"; readonly seconds: number }
  /** Ставить тень. */
  | { readonly kind: "hold" };

export function pageReturnStep(facts: ReturnVideoFacts, target: PageReturnTarget): PageReturnStep {
  if (facts.readyState < HAVE_METADATA) return { kind: "wait" };
  // Эфир — `Infinity` или длиннее года (решение 319); NaN — метаданных нет, ждём.
  if (target.kind === "live") {
    const recorded = Number.isFinite(facts.durationS) && !isEndlessDurationSeconds(facts.durationS);
    return recorded ? { kind: "not_this_video" } : { kind: "hold" };
  }
  if (!Number.isFinite(facts.durationS) || Math.abs(facts.durationS - target.duration_s) > PAGE_RETURN_DURATION_TOLERANCE_S) {
    return { kind: "not_this_video" };
  }
  if (facts.seeking) return { kind: "wait" };
  if (Math.abs(facts.currentTimeS - target.start_s) <= RETURN_POSITION_SETTLE_S) return { kind: "hold" };
  if (facts.seeksIssued >= MAX_RETURN_SEEKS) return { kind: "hold" };
  return { kind: "seek_to", seconds: target.start_s };
}
