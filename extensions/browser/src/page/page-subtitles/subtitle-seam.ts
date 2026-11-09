// Шов между контроллером передачи и субтитрами страницы (сессия 13b2).
//
// Контроллер знает только эти три вещи: «передача началась — вот элемент и как отправлять»,
// «тик часов плеера» и «передача кончилась». Склад документов, хуки сети, `<track>` и сборка WebVTT
// ему не видны: подменяются целиком, и контроллер не меняется.

import type { PageSubtitleEvent } from "../../shared/page-subtitle-contract.ts";
import type { ReferrerFacts } from "./track-watch.ts";

/** Что контроллер даёт субтитрам на время одной передачи. */
export interface PageSubtitleHandoffContext {
  /** Переданный элемент `<video>`. */
  readonly element: object;
  /** Факты Referer для `<track src>` (решение 202): считает контроллер, как для `file_source`. */
  readonly referrerFacts: () => ReferrerFacts;
  /**
   * Отправить событие фону. Контроллер сам проверяет, что передача ещё та же, и после её конца ничего не
   * шлёт. Может бросить (порт отказал) — субтитры ловят это сами и передачу не рвут.
   */
  readonly send: (event: PageSubtitleEvent, transfer: readonly Transferable[]) => void;
}

/** Субтитры одной идущей передачи. */
export interface PageSubtitleHandoff {
  /** Тик часов плеера (раз в ~250 мс): повод проверить рост реплик, не чаще раза в 5 с. */
  clockTick(): void;
  /** Передача кончилась: слушатели сняты, дальше ничего не отправляется. */
  end(): void;
}

/** Субтитры документа страницы: склад живёт весь документ, отдача — на время передачи. */
export interface PageSubtitleSeam {
  /** Передача началась: отдать склад и начать следить за `<track>`/`TextTrack` элемента. */
  begin(context: PageSubtitleHandoffContext): PageSubtitleHandoff;
}
