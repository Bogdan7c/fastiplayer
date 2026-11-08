// Сигнал предложения «Ask» (сессия 12b), сторона изолированного мира. Пара к `page/viewer-offer-anchor.ts`.
//
// Имя события — производное от тайного имени якоря заглушки (`viewerOfferEventNameOf`): его выбрал этот мир, и
// MAIN-мир получил его тем же сообщением `watch_handed_off_video`. Значит, слушатель надо поставить ДО того,
// как это сообщение уйдёт, — тогда первый сигнал не потеряется. Разбор цели — тот же, что у якоря заглушки.
import { viewerOfferEventNameOf } from "../shared/handed-off-video-contract.ts";
import { isHandedOffVideoElement, signalTargetOf, type AnchorSignalEvent } from "./handed-off-video-listener.ts";
import type { ViewerOfferSink } from "./page-ui/viewer-offer-controller.ts";

/**
 * Слушать сигналы предложения. `addCaptureListener` — слушатель на своём `window` в фазе захвата. Возвращает,
 * сколько сигналов пришло с целью, не похожей на элемент (для тестов и диагностики).
 */
export function listenForViewerOffer(
  addCaptureListener: (eventName: string, listener: (event: AnchorSignalEvent) => void) => void,
  anchorEventName: string,
  sink: ViewerOfferSink,
): { rejectedSignals(): number } {
  let rejected = 0;
  addCaptureListener(viewerOfferEventNameOf(anchorEventName), (event) => {
    // Сигнал наш и только наш: странице его не видно ни на одном узле ниже `window`.
    event.stopImmediatePropagation();
    // Разослан на самом `window` — «предложение ушло».
    if (event.target === event.currentTarget) {
      sink.viewerOfferLeft();
      return;
    }
    const target = signalTargetOf(event);
    if (isHandedOffVideoElement(target)) sink.viewerOfferStood(target);
    else rejected += 1;
  });
  return { rejectedSignals: () => rejected };
}
