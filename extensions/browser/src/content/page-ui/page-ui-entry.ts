// Переходники слоя страницы к настоящим API браузера (изолированный мир, все фреймы). Логика — в
// `page-ui.ts` и соседях; здесь только подключение.
//
// До включения настройки и до первого сообщения фона модуль не делает ничего заметного: ни узлов в
// документе, ни слушателей мыши, ни сообщений фону (чтение `storage.local` — не сообщение фону).
import { browserApi } from "../../shared/browser-api.ts";
import { PAGE_UI_PROBE_FUNCTION_NAME } from "../../shared/entry-contract.ts";
import type { OverlayButtonPressedMessage, VideoAtPoint } from "../../shared/entry-contract.ts";
import { readEntrySettings } from "../../shared/entry-settings.ts";
import type { HandedOffVideoSink } from "../handed-off-video-listener.ts";
import type { ViewerReturnDelivery } from "../relay.ts";
import { createDocumentPointerSource } from "./document-pointer-source.ts";
import { createBrowserGeometrySignals } from "./placeholder-geometry.ts";
import { createPageUi } from "./page-ui.ts";
import { createRealUiDocument } from "./real-ui-document.ts";

/** Что слою страницы нужно от пограничника (relay.ts). */
export interface PageUiRelayPorts {
  readonly videoAtPoint: (x: number, y: number) => Promise<VideoAtPoint | null>;
  readonly requestViewerReturn: () => ViewerReturnDelivery;
}

/** Отметка для разработчика (журнала в продукте нет). */
function noteForDeveloper(message: string, error: unknown): void {
  console.debug(`[fastiplayer] ${message}:`, error);
}

/**
 * Подключить слой страницы к этому документу; зовётся один раз при загрузке контент-скрипта. Возвращает,
 * куда якорь сообщает о переданном видео (заглушка, 11a).
 */
export function installPageUi(relay: PageUiRelayPorts): HandedOffVideoSink {
  const { videoAtPoint } = relay;
  const pageUi = createPageUi({
    document: createRealUiDocument(document),
    viewportSize: () => ({ width: window.innerWidth, height: window.innerHeight }),
    pointer: createDocumentPointerSource(document),
    timers: {
      setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
      clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    },
    now: () => performance.now(),
    ownExtensionId: browserApi.runtime.id,
    isTopFrame: window === window.top,
    videoAtPoint,
    // Фон мог уснуть: отказ доставки ловит контроллер и пишет для разработчика.
    sendPressed: (message: OverlayButtonPressedMessage) => Promise.resolve(browserApi.runtime.sendMessage(message)),
    readSettings: () => readEntrySettings(browserApi.storage.local),
    onLocalStorageChanged: (listener) => {
      const handler = (changes: Record<string, chrome.storage.StorageChange>, areaName: string): void => {
        if (areaName === "local") listener(changes);
      };
      browserApi.storage.onChanged.addListener(handler);
      return () => browserApi.storage.onChanged.removeListener(handler);
    },
    note: noteForDeveloper,
    placeholderSignals: createBrowserGeometrySignals(window, noteForDeveloper),
    requestViewerReturn: relay.requestViewerReturn,
  });
  browserApi.runtime.onMessage.addListener((message: unknown, sender) => pageUi.onMessage(message, sender));

  // Зонд стенда: только чтение, глобал изолированного мира — странице не виден.
  Object.defineProperty(globalThis, PAGE_UI_PROBE_FUNCTION_NAME, {
    value: () => pageUi.probe(),
    configurable: false,
    enumerable: false,
    writable: false,
  });
  return pageUi.handedOffVideo;
}
