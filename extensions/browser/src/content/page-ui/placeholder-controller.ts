// Решения заглушки «Playing in Fastiplayer» (сессия 11a, решение 236).
//
// Жизнь заглушки = жизнь активной передачи в MAIN-мире: «видео передано» (сигнал якоря с элементом) — она
// есть, «передача ушла» — её нет. Между роликами (09c2) передачи нет, значит, нет и заглушки.
//
// Пока передача есть, заглушка стоит ровно над прямоугольником элемента и следует за ним (сигналы
// `placeholder-geometry.ts` и периодический перемер `PLACEHOLDER_LAYOUT_RECHECK_MS` — для сдвигов вёрстки без
// событий). Элемент убран из документа или стал нулевого размера — заглушка прячется; вернулся, пока передача
// идёт, — показывается снова.
//
// Передача ушла — заглушка не исчезает мгновенно: ещё `PLACEHOLDER_LEAVE_CLICK_SHIELD_MS` она стоит невидимым
// щитом и гасит клики по месту видео. Человек мог жать «Return» ровно тогда, когда закрыл окно плеера: без
// щита запоздалый клик попал бы в родной плеер сайта, и тот запустил бы видео, оставленное на паузе (стенд
// 11a: клик через ~130 мс после закрытия, нативные `controls` запускали игру в обоих браузерах).
//
// «Return» принимается только от настоящего клика (`isTrusted`: скрипт страницы его не подделает) и уходит
// фону не больше одного раза за передачу. Просьба, которая не ушла (передачи с живым портом ещё нет), повтор
// не запрещает: иначе кнопка молча умерла бы.
import type { PageUiRect, PlaceholderProbe } from "../../shared/entry-contract.ts";
import type { HandedOffVideoElement, HandedOffVideoSink } from "../handed-off-video-listener.ts";
import type { ViewerReturnDelivery } from "../relay.ts";
import type { OverlayButtonTimers } from "./overlay-button-controller.ts";
import type { PlaceholderGeometryChange, PlaceholderGeometrySignals } from "./placeholder-geometry.ts";
import type { PlaceholderView } from "./placeholder-view.ts";
import type { UiEvent } from "./ui-dom.ts";

/**
 * Как часто перемерять прямоугольник видео без всяких событий, мс. Сдвиг вёрстки (вставленный баннер, выросший
 * сосед) событий не даёт; 250 мс — заметно глазу не дольше мгновения, а цена — одно чтение прямоугольника.
 * В скрытой вкладке браузер душит таймеры — и хорошо: рисовать там нечего.
 */
export const PLACEHOLDER_LAYOUT_RECHECK_MS = 250;

/**
 * Сколько после конца передачи заглушка гасит клики невидимым щитом, мс. Запоздалый клик на стенде приходил
 * через ~130 мс после закрытия окна; 300 мс накрывают его с запасом, а человек, только что закрывший окно,
 * такую задержку отклика сайта не заметит.
 */
export const PLACEHOLDER_LEAVE_CLICK_SHIELD_MS = 300;

export interface PlaceholderDependencies {
  readonly view: PlaceholderView;
  readonly signals: PlaceholderGeometrySignals;
  readonly timers: OverlayButtonTimers;
  /** Передать фону просьбу «Return» (пограничник, `relay.requestViewerReturn`). */
  readonly requestViewerReturn: () => ViewerReturnDelivery;
  readonly note: (message: string, error: unknown) => void;
}

export interface PlaceholderController extends HandedOffVideoSink {
  /** Что видно сейчас (для стенда). */
  probe(): PlaceholderProbe;
}

/** Передача, над которой стоит заглушка. */
interface PlaceholderSession {
  readonly element: HandedOffVideoElement;
  readonly stopSignals: () => void;
  recheckTimer: unknown;
  /** Просьба «Return» за эту передачу уже ушла фону. */
  returnRequested: boolean;
}

export function createPlaceholderController(dependencies: PlaceholderDependencies): PlaceholderController {
  const { view, timers } = dependencies;
  let session: PlaceholderSession | null = null;
  /** Таймер снятия щита после конца передачи; `null` — щита нет. */
  let leaveShieldTimer: unknown = null;

  function cancelLeaveShield(): void {
    if (leaveShieldTimer === null) return;
    timers.clearTimeout(leaveShieldTimer);
    leaveShieldTimer = null;
  }

  /** Конец передачи: показанная заглушка становится щитом и прячется по таймеру; не показанная — сразу. */
  function leaveBehindShield(): void {
    cancelLeaveShield();
    if (view.shieldClicksWhileLeaving() === "nothing_shown") {
      view.hide();
      return;
    }
    leaveShieldTimer = timers.setTimeout(() => {
      leaveShieldTimer = null;
      view.hide();
    }, PLACEHOLDER_LEAVE_CLICK_SHIELD_MS);
  }

  /** Прямоугольник, над которым можно рисовать; `null` — элемент вне документа, нулевой или не читается. */
  function drawableRectOf(element: HandedOffVideoElement): PageUiRect | null {
    try {
      if (!element.isConnected) return null;
      const rect = element.getBoundingClientRect();
      if (!(rect.width > 0 && rect.height > 0)) return null;
      return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
    } catch (error) {
      dependencies.note("placeholder could not measure the video", error);
      return null;
    }
  }

  /** Перемерить и поставить заглушку там, где элемент сейчас (или спрятать). */
  function follow(current: PlaceholderSession): void {
    const rect = drawableRectOf(current.element);
    if (rect === null) view.hide();
    else view.show(rect, onReturnClicked);
  }

  function scheduleRecheck(current: PlaceholderSession): void {
    current.recheckTimer = timers.setTimeout(() => {
      if (session !== current) return;
      follow(current);
      scheduleRecheck(current);
    }, PLACEHOLDER_LAYOUT_RECHECK_MS);
  }

  function onGeometryChange(current: PlaceholderSession, change: PlaceholderGeometryChange): void {
    if (session !== current) return;
    follow(current);
    // Страница ушла в полноэкранный режим (или вышла): её элемент теперь в верхнем слое — встаём над ним.
    if (change === "fullscreen_changed") view.raise();
  }

  function onReturnClicked(event: UiEvent): void {
    // Синтетический клик (скрипт страницы, `element.click()`) — не решение человека.
    if (!event.isTrusted) return;
    const current = session;
    if (current === null || current.returnRequested) return;
    const delivery = dependencies.requestViewerReturn();
    if (delivery !== "sent") {
      dependencies.note("viewer return was not delivered", delivery);
      return;
    }
    current.returnRequested = true;
    view.markReturnRequested(true);
  }

  function stop(): void {
    const current = session;
    if (current === null) return;
    session = null;
    timers.clearTimeout(current.recheckTimer);
    current.stopSignals();
    leaveBehindShield();
  }

  function videoHandedOff(element: HandedOffVideoElement): void {
    if (session?.element === element) {
      follow(session);
      return;
    }
    // Новая передача без «ушло» старой (сигналы упорядочены, но лишняя осторожность ничего не стоит).
    stop();
    // Щит прошлой передачи больше не нужен: панель сейчас встанет над новым видео.
    cancelLeaveShield();
    let current: PlaceholderSession | null = null;
    const stopSignals = dependencies.signals.watch(element, (change) => {
      if (current !== null) onGeometryChange(current, change);
    });
    current = { element, stopSignals, recheckTimer: undefined, returnRequested: false };
    session = current;
    view.markReturnRequested(false);
    follow(current);
    scheduleRecheck(current);
  }

  return {
    videoHandedOff,
    videoLeft: stop,
    probe() {
      const measured = view.measure();
      return { handed_off: session !== null, visible: measured.panel !== null, panel: measured.panel, return_button: measured.returnButton };
    },
  };
}
