// Решения панели предложения «Ask» (сессия 12b, решения 255–257).
//
// Жизнь панели = жизнь предложения в MAIN-мире: «предложение встало» (сигнал с элементом) — она есть,
// «предложение ушло» — её нет. Пока панель есть, она стоит ровно над видео и следует за ним (те же сигналы
// геометрии и тот же перемер, что у заглушки «Playing in Fastiplayer»).
//
// Нажатие:
// - принимается только от настоящего клика (`isTrusted`): скрипт страницы его не подделает;
// - одно за предложение: после него кнопки гаснут, а для «в плеер» и «Always» появляется «Opening in
//   Fastiplayer…» — плеер стартует до нескольких секунд;
// - уходит пограничнику (`answerViewerOffer`), а тот — фону. Панель сама ничего не решает и ничего не пишет:
//   уйдёт она по сигналу MAIN-мира, когда видео отпущено или забрано в плеер.
//
// Предложение ушло — панель ещё `PLACEHOLDER_LEAVE_CLICK_SHIELD_MS` гасит клики невидимым щитом: двойной клик
// по «Play here» иначе вторым нажатием попал бы в плеер сайта и поставил бы только что запущенное видео на паузу.
import type { PageUiRect, ViewerOfferProbe } from "../../shared/entry-contract.ts";
import type { ViewerOfferChoice } from "../../shared/viewer-offer-contract.ts";
import type { HandedOffVideoElement } from "../handed-off-video-listener.ts";
import type { ViewerOfferDelivery } from "../viewer-offer-relay.ts";
import type { OverlayButtonTimers } from "./overlay-button-controller.ts";
import { PLACEHOLDER_LAYOUT_RECHECK_MS, PLACEHOLDER_LEAVE_CLICK_SHIELD_MS } from "./placeholder-controller.ts";
import type { PlaceholderGeometryChange, PlaceholderGeometrySignals } from "./placeholder-geometry.ts";
import type { UiEvent } from "./ui-dom.ts";
import type { ViewerOfferView } from "./viewer-offer-view.ts";

/** Кто узнаёт о предложении (сигналы якоря предложения изолированного мира). */
export interface ViewerOfferSink {
  viewerOfferStood(element: HandedOffVideoElement): void;
  viewerOfferLeft(): void;
}

export interface ViewerOfferControllerDependencies {
  readonly view: ViewerOfferView;
  readonly signals: PlaceholderGeometrySignals;
  readonly timers: OverlayButtonTimers;
  /** Передать выбор пограничнику (`relay.answerViewerOffer`). */
  readonly answerViewerOffer: (choice: ViewerOfferChoice) => Promise<ViewerOfferDelivery>;
  readonly note: (message: string, error: unknown) => void;
}

export interface ViewerOfferController extends ViewerOfferSink {
  probe(): ViewerOfferProbe;
}

/** Выборы, после которых видео уходит в плеер: панель показывает «Opening in Fastiplayer…». */
const OPENS_PLAYER: ReadonlySet<ViewerOfferChoice> = new Set(["fastiplayer", "always"]);

interface OfferSession {
  readonly element: HandedOffVideoElement;
  readonly stopSignals: () => void;
  recheckTimer: unknown;
  /** Выбор за это предложение уже ушёл. */
  answered: boolean;
}

export function createViewerOfferController(dependencies: ViewerOfferControllerDependencies): ViewerOfferController {
  const { view, timers } = dependencies;
  let session: OfferSession | null = null;
  let leaveShieldTimer: unknown = null;

  function cancelLeaveShield(): void {
    if (leaveShieldTimer === null) return;
    timers.clearTimeout(leaveShieldTimer);
    leaveShieldTimer = null;
  }

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

  function drawableRectOf(element: HandedOffVideoElement): PageUiRect | null {
    try {
      if (!element.isConnected) return null;
      const rect = element.getBoundingClientRect();
      if (!(rect.width > 0 && rect.height > 0)) return null;
      return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
    } catch (error) {
      dependencies.note("viewer offer could not measure the video", error);
      return null;
    }
  }

  function follow(current: OfferSession): void {
    const rect = drawableRectOf(current.element);
    if (rect === null) {
      view.hide();
      return;
    }
    // Слоя нет — панель не нарисовать, а видео стоит на паузе: человеку нечем его запустить. Играем здесь.
    if (view.show(rect, onChoice) === "no_layer" && !current.answered) {
      dependencies.note("viewer offer could not be drawn, the video plays here", current.element);
      answer(current, "here");
    }
  }

  function scheduleRecheck(current: OfferSession): void {
    current.recheckTimer = timers.setTimeout(() => {
      if (session !== current) return;
      follow(current);
      scheduleRecheck(current);
    }, PLACEHOLDER_LAYOUT_RECHECK_MS);
  }

  function onGeometryChange(current: OfferSession, change: PlaceholderGeometryChange): void {
    if (session !== current) return;
    follow(current);
    if (change === "fullscreen_changed") view.raise();
  }

  function onChoice(choice: ViewerOfferChoice, event: UiEvent): void {
    if (!event.isTrusted) return;
    const current = session;
    if (current === null || current.answered) return;
    answer(current, choice);
  }

  function answer(current: OfferSession, choice: ViewerOfferChoice): void {
    current.answered = true;
    view.setState(OPENS_PLAYER.has(choice) ? "opening" : "answered");
    void dependencies.answerViewerOffer(choice).then((delivery) => {
      // Видео при этих исходах уже отпущено пограничником и играет здесь: панель уйдёт по сигналу MAIN-мира.
      if (delivery === "no_offer" || delivery === "send_failed") dependencies.note("viewer offer choice was not taken", delivery);
    });
  }

  function stop(): void {
    const current = session;
    if (current === null) return;
    session = null;
    timers.clearTimeout(current.recheckTimer);
    current.stopSignals();
    leaveBehindShield();
  }

  function viewerOfferStood(element: HandedOffVideoElement): void {
    if (session?.element === element) {
      follow(session);
      return;
    }
    stop();
    cancelLeaveShield();
    let current: OfferSession | null = null;
    const stopSignals = dependencies.signals.watch(element, (change) => {
      if (current !== null) onGeometryChange(current, change);
    });
    current = { element, stopSignals, recheckTimer: undefined, answered: false };
    session = current;
    view.setState("asking");
    follow(current);
    scheduleRecheck(current);
  }

  return {
    viewerOfferStood,
    viewerOfferLeft: stop,
    probe() {
      const measured = view.measure();
      return { offered: session !== null, answered: session?.answered === true, visible: measured.panel !== null, panel: measured.panel, buttons: measured.buttons };
    },
  };
}
