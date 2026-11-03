// Play/pause зрителя из окна плеера доходит до страницы «как медиаклавиша» (решение 194).
//
// Почему не просто событие элементу. Twitch (живой эфир) на свою паузу «выгружает» эфир:
// `pause()`, удаляет весь буфер и ставит `currentTime = 0`. Снова загружает он эфир только из
// своего обработчика Media Session `play` — так работает системная медиаклавиша браузера. Если же
// тень просто показывает странице `play`/`playing` на элементе, плеер сайта в состоянии
// «выгружено» тут же зовёт `pause()` и эфир стоит навсегда (Chromium) или оживает рывками (Firefox).
//
// Правило (как у браузера на системную клавишу):
//  - страница поставила обработчик этого действия → зовём ЕГО и больше ничего не делаем: дальше
//    страница сама решает, что делать с элементом (обычно зовёт `play()`/`pause()` — это её
//    просьба, она уходит плееру обычным `page_playback`, а плеер узнаёт в ней подтверждение
//    своей команды и второй команды не шлёт — «правило эха» плеера);
//  - обработчика нет → как раньше: тень сама ставит игру/паузу на элементе;
//  - обработчик бросил → одна строка отладки и тоже как раньше (иначе нажатие зрителя пропало
//    бы молча), но только если тень ещё жива: обработчик мог успеть забрать элемент у тени.
//
// Плееру между вызовом обработчика и собственным `play()`/`pause()` страницы ничего не уходит:
// плеер уже в том состоянии, которое выбрал зритель (он сам и прислал команду).

import type { MediaKeyAction, PageActionOutcome } from "../media-session-guard.ts";

/** Куда в итоге попало нажатие зрителя (для проверок и отладки). */
export type ViewerMediaKeyRoute =
  /** Обработчик Media Session страницы вызван; элемент тень не трогала. */
  | "page_handler"
  /** Тень сама поставила игру/паузу на элементе (обработчика нет или он упал). */
  | "element"
  /** Обработчик упал, а тень к этому моменту уже снята — поставить игру/паузу некому. */
  | "shadow_gone";

/** Что нужно доставке от тени и стража (шов: в тестах — фейки). */
export interface ViewerMediaKeyPorts {
  /** Вызвать обработчик страницы (владелец обработчиков — страж mediaSession). */
  readonly invokePageAction: (action: MediaKeyAction) => PageActionOutcome;
  /** Прежняя доставка тенью: игра/пауза на элементе по команде плеера. */
  readonly deliverToElement: (action: MediaKeyAction) => void;
  /** Тень ещё обслуживает элемент (обработчик страницы мог её снять). */
  readonly shadowActive: () => boolean;
  /** Единственная отладочная строка документа (once-reporter). */
  readonly reportProblem: (context: string, problem: unknown) => void;
}

/** Доставить play/pause зрителя странице по правилу медиаклавиши (см. шапку модуля). */
export function deliverViewerMediaKey(action: MediaKeyAction, ports: ViewerMediaKeyPorts): ViewerMediaKeyRoute {
  const outcome = ports.invokePageAction(action);
  switch (outcome.kind) {
    case "invoked":
      return "page_handler";
    case "no_handler":
      ports.deliverToElement(action);
      return "element";
    case "handler_failed":
      // Сбой обработчика страницы — не наш сбой, но нажатие зрителя не должно пропасть:
      // откат на прежнюю доставку элементу и одна строка отладки за жизнь документа.
      ports.reportProblem(`mediaSession ${action} handler threw; delivered to the element instead`, outcome.problem);
      if (!ports.shadowActive()) return "shadow_gone";
      ports.deliverToElement(action);
      return "element";
  }
}
