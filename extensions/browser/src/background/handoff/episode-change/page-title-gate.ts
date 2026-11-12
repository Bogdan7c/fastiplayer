// Ворота заголовка вкладки перед `page_title_changed` (сессия 16a3, находка 2 живой проверки 16a2):
// заголовок окна плеера меняется, только когда ясно, что заголовок относится к видео на экране.
//
// Дефект: зритель кликнул логотип YouTube (уход на главную одностраничника), браузер тут же сообщил
// заголовок «YouTube», и окно его показало, хотя плеер ещё 30 с досматривал прежний ролик. Только через
// 5 с экстеншен признал уход страницы (`tab_gone{navigated_away}`). Заголовок шёл мимо единственного
// знания, которое отличает «новая серия» от «страница ушла»: адреса вкладки с начала видео.
//
// Правила (владелец — этот модуль; частоту и чистку ведёт page-title-relay.ts):
// - новый заголовок ждёт `PAGE_TITLE_SETTLE_MS`: адрес и заголовок одной навигации браузер сообщает
//   отдельными событиями `tabs.onUpdated` и в любом порядке, а судить можно только по обоим;
// - адрес с начала видео тот же → заголовок уходит (смена серии без смены адреса, счётчик «(3)» и т. п.);
// - адрес сменился → заголовок ждёт: началось новое видео — уходит самый свежий; страница ушла и сеанс
//   кончился (`stop`) — заголовок выброшен, окно до конца досмотра держит название прежнего ролика;
// - ждёт не дольше `PAGE_TITLE_AWAY_HOLD_MS`: так бывает, когда новое видео началось РАНЬШЕ смены адреса
//   (опорный адрес остался от прошлого ролика, и вердикт «сменился» уже не снимется). Уход страницы к
//   этому сроку уже признан (`tab_gone`), значит, видео живо, и заголовок — его;
// - ждёт не больше одного заголовка (самый свежий), память ограничена по построению.

import type { Scheduler } from "../../host-link/native-port.ts";
import { createTimerSlot } from "../../host-link/timer-slot.ts";
import type { PageAddressSinceVideoBegan } from "./page-address-tracker.ts";

/**
 * Сколько заголовок ждёт адрес той же навигации, мс. 1 с: одностраничник меняет адрес (`pushState`) и
 * заголовок в пределах сотен миллисекунд, а смену заголовка окна человек замечает с задержкой в
 * секунду-другую — задержка незаметна, а ложный заголовок досмотра — заметен.
 */
export const PAGE_TITLE_SETTLE_MS = 1_000;

/**
 * Дольше этого заголовок при сменённом адресе не ждёт, мс. Уход страницы признаётся через допуск пустого
 * источника тени посреди серии (5 с, `EMPTY_SOURCE_GRACE_MS` мира страницы) по тикам часов (раз в ~250 мс)
 * и доходит до фона; 8 с — этот допуск с запасом в 3 с. Связь констант закреплена тестом.
 */
export const PAGE_TITLE_AWAY_HOLD_MS = 8_000;

export interface PageTitleGatePorts {
  /** Часы и таймеры фона (в тестах — фейковые). */
  readonly scheduler: Scheduler;
  /** Сменился ли адрес вкладки с начала текущего видео (page-address-tracker.ts). */
  readonly addressSinceVideoBegan: () => PageAddressSinceVideoBegan;
  /** Отдать заголовок дальше — реле частоты и чистки (`PageTitleRelay.titleSeen`). */
  readonly relay: (rawTitle: string) => void;
}

export interface PageTitleGate {
  /** Браузер сообщил новый заголовок вкладки сеанса (сырой). */
  titleSeen(rawTitle: string): void;
  /** На экране плеера началось новое видео или поколение: ждущий заголовок — его. */
  videoBegan(): void;
  /** Сеанс кончен: ждущий заголовок выброшен, таймер снят. Повторный вызов безвреден. */
  stop(): void;
}

export function createPageTitleGate(ports: PageTitleGatePorts): PageTitleGate {
  const settleTimer = createTimerSlot(ports.scheduler);
  const awayHoldTimer = createTimerSlot(ports.scheduler);
  // Самый свежий заголовок, ещё не отданный реле (`undefined` — ждущего нет).
  let waitingTitle: string | undefined;

  const relayWaiting = (): void => {
    awayHoldTimer.cancel();
    const title = waitingTitle;
    waitingTitle = undefined;
    if (title !== undefined) ports.relay(title);
  };

  // Адрес той же навигации уже успел прийти: решаем по нему.
  const decide = (): void => {
    if (ports.addressSinceVideoBegan() === "changed") {
      // Ждём нового видео или конца сеанса, но не дольше срока (видео могло начаться раньше смены адреса).
      if (!awayHoldTimer.isActive()) awayHoldTimer.start(PAGE_TITLE_AWAY_HOLD_MS, relayWaiting);
      return;
    }
    relayWaiting();
  };

  return {
    titleSeen(rawTitle) {
      waitingTitle = rawTitle;
      // Окно отсчитывается от первого неотданного заголовка: «бегущий» заголовок не держит его вечно.
      if (!settleTimer.isActive()) settleTimer.start(PAGE_TITLE_SETTLE_MS, decide);
    },
    videoBegan() {
      settleTimer.cancel();
      relayWaiting();
    },
    stop() {
      settleTimer.cancel();
      awayHoldTimer.cancel();
      waitingTitle = undefined;
    },
  };
}
