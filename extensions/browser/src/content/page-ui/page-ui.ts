// Сборка слоя страницы: общий слой + плашка + кнопка поверх видео + слежение за настройкой + зонд.
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
}

export interface PageUi {
  /** Слушатель для `runtime.onMessage`: плашка от фона. */
  readonly onMessage: (message: unknown, sender: MessageSenderFacts) => void;
  /** Что показано сейчас — читается с экрана, не из «намерений» (для стенда). */
  readonly probe: () => PageUiProbe;
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
  watchOverlaySetting({
    readSettings: dependencies.readSettings,
    onLocalStorageChanged: dependencies.onLocalStorageChanged,
    applyEnabled: (enabled) => button.setEnabled(enabled),
    note: dependencies.note,
  });
  return {
    onMessage: createBannerMessageListener({ ownExtensionId: dependencies.ownExtensionId, isTopFrame: dependencies.isTopFrame, banner }),
    probe: () => ({ banner: banner.shownId(), overlay_button: button.measureButton() }),
  };
}
