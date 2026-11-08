// Сборка слоя страницы: общий слой + плашка + кнопка поверх видео + заглушка над переданным видео (11a) +
// панель предложения «Ask» над замершим видео (12b) +
// слежение за настройкой + зонд.
// Все внешние миры (DOM, мышь, таймеры, хранилище, фон) приходят зависимостями: сборка проверяется в
// node:test целиком, без браузера. Настоящие переходники — в `page-ui-entry.ts`.
import type { OverlayButtonPressedMessage, PageUiProbe, VideoAtPoint } from "../../shared/entry-contract.ts";
import type { EntrySettings } from "../../shared/entry-settings.ts";
import { createBannerController } from "./banner-controller.ts";
import { createBannerMessageListener, type MessageSenderFacts } from "./banner-listener.ts";
import { createBannerView } from "./banner-view.ts";
import type { ViewportSize } from "./button-placement.ts";
import { createOverlayButtonController, type OverlayButtonTimers, type PointerSource } from "./overlay-button-controller.ts";
import { createOverlayButtonView } from "./overlay-button-view.ts";
import type { HandedOffVideoSink } from "../handed-off-video-listener.ts";
import type { ViewerReturnDelivery } from "../relay.ts";
import { createPlaceholderController } from "./placeholder-controller.ts";
import type { PlaceholderGeometrySignals } from "./placeholder-geometry.ts";
import { createPlaceholderView } from "./placeholder-view.ts";
import type { ViewerOfferChoice } from "../../shared/viewer-offer-contract.ts";
import type { ViewerOfferDelivery } from "../viewer-offer-relay.ts";
import { createViewerOfferController, type ViewerOfferSink } from "./viewer-offer-controller.ts";
import { createViewerOfferView } from "./viewer-offer-view.ts";
import { watchOverlaySetting } from "./overlay-setting-watch.ts";
import type { UiDocument } from "./ui-dom.ts";
import { createUiLayer } from "./ui-layer.ts";

export interface PageUiDependencies {
  readonly document: UiDocument;
  readonly viewportSize: () => ViewportSize;
  readonly pointer: PointerSource;
  readonly timers: OverlayButtonTimers;
  readonly now: () => number;
  readonly ownExtensionId: string;
  readonly isTopFrame: boolean;
  readonly videoAtPoint: (x: number, y: number) => Promise<VideoAtPoint | null>;
  readonly sendPressed: (message: OverlayButtonPressedMessage) => Promise<unknown>;
  readonly readSettings: () => Promise<EntrySettings>;
  readonly onLocalStorageChanged: (listener: (changes: Readonly<Record<string, { readonly newValue?: unknown }>>) => void) => () => void;
  readonly note: (message: string, error: unknown) => void;
  /** Сигналы «видео могло сдвинуться» для заглушки. */
  readonly placeholderSignals: PlaceholderGeometrySignals;
  /** Просьба «Return» с заглушки — фону через пограничника. */
  readonly requestViewerReturn: () => ViewerReturnDelivery;
  /** Выбор на предложении «Ask» (12b) — фону через пограничника. */
  readonly answerViewerOffer: (choice: ViewerOfferChoice) => Promise<ViewerOfferDelivery>;
}

export interface PageUi {
  /** Слушатель для `runtime.onMessage`: плашка от фона. */
  readonly onMessage: (message: unknown, sender: MessageSenderFacts) => void;
  /** Что показано сейчас — читается с экрана, не из «намерений» (для стенда). */
  readonly probe: () => PageUiProbe;
  /** Куда якорь изолированного мира сообщает о переданном видео (заглушка). */
  readonly handedOffVideo: HandedOffVideoSink;
  /** Куда сигнал предложения сообщает о замершем видео (панель «Ask», 12b). */
  readonly viewerOffer: ViewerOfferSink;
}

export function createPageUi(dependencies: PageUiDependencies): PageUi {
  const layer = createUiLayer(dependencies.document);
  const banner = createBannerController(createBannerView(dependencies.document, layer), dependencies.timers);
  const button = createOverlayButtonController({
    pointer: dependencies.pointer,
    view: createOverlayButtonView(dependencies.document, layer, dependencies.viewportSize),
    timers: dependencies.timers,
    now: dependencies.now,
    isOwnElement: (target) => target !== null && target === layer.host(),
    videoAtPoint: dependencies.videoAtPoint,
    sendPressed: dependencies.sendPressed,
    note: dependencies.note,
  });
  const placeholder = createPlaceholderController({
    view: createPlaceholderView(dependencies.document, layer),
    signals: dependencies.placeholderSignals,
    timers: dependencies.timers,
    requestViewerReturn: dependencies.requestViewerReturn,
    note: dependencies.note,
  });
  const viewerOffer = createViewerOfferController({
    view: createViewerOfferView(dependencies.document, layer),
    signals: dependencies.placeholderSignals,
    timers: dependencies.timers,
    answerViewerOffer: dependencies.answerViewerOffer,
    note: dependencies.note,
  });
  watchOverlaySetting({
    readSettings: dependencies.readSettings,
    onLocalStorageChanged: dependencies.onLocalStorageChanged,
    applyEnabled: (enabled) => button.setEnabled(enabled),
    note: dependencies.note,
  });
  return {
    onMessage: createBannerMessageListener({ ownExtensionId: dependencies.ownExtensionId, isTopFrame: dependencies.isTopFrame, banner }),
    probe: () => ({ banner: banner.shownId(), overlay_button: button.measureButton(), placeholder: placeholder.probe(), viewer_offer: viewerOffer.probe() }),
    handedOffVideo: placeholder,
    viewerOffer,
  };
}
