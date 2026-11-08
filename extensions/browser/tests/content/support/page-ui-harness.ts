// Стенд для проверки слоя страницы целиком: фейковые DOM, мышь, часы, хранилище и «MAIN-мир».
import type { OverlayButtonPressedMessage, VideoAtPoint } from "../../../src/shared/entry-contract.ts";
import { ENTRY_SETTINGS_KEY, parseEntrySettings } from "../../../src/shared/entry-settings.ts";
import { createPageUi, type PageUi } from "../../../src/content/page-ui/page-ui.ts";
import { createFakeClock, type FakeClock } from "../../background/support.ts";
import type { ViewerReturnDelivery } from "../../../src/content/relay.ts";
import type { ViewerOfferDelivery } from "../../../src/content/viewer-offer-relay.ts";
import type { ViewerOfferChoice } from "../../../src/shared/viewer-offer-contract.ts";
import { FakeGeometrySignals, FakePointer, FakeUiDocument, flushMicrotasks } from "./fake-ui-dom.ts";

export const OWN_EXTENSION_ID = "fastiplayer-extension-id";
export const VIDEO_AT_POINT: VideoAtPoint = {
  document: "doc-1",
  element: 7,
  rect: { left: 100, top: 50, width: 640, height: 360 },
  in_player: false,
};

export interface PageUiHarness {
  readonly ui: PageUi;
  readonly doc: FakeUiDocument;
  readonly pointer: FakePointer;
  readonly clock: FakeClock;
  /** Что ушло фону. */
  readonly sent: OverlayButtonPressedMessage[];
  /** Вопросы к MAIN-миру. */
  readonly asked: { x: number; y: number }[];
  /** Что ответит MAIN-мир на следующий вопрос (по умолчанию видео под любой точкой). */
  answer: { next: () => Promise<VideoAtPoint | null> };
  /** Живые подписки на `storage.onChanged`. */
  readonly storageListeners: Set<(changes: Record<string, { newValue?: unknown }>) => void>;
  /** Сигналы «видео сдвинулось» для заглушки (тест зовёт их сам). */
  readonly signals: FakeGeometrySignals;
  /** Сколько раз заглушка просила «Return» у пограничника. */
  readonly returnRequests: { count: number };
  /** Что ответит пограничник на следующую просьбу «Return». */
  returnDelivery: { next: ViewerReturnDelivery };
  /** Что панель предложения «Ask» отдала пограничнику (12b). */
  readonly offerChoices: ViewerOfferChoice[];
  /** Что ответит пограничник на следующий выбор. */
  offerDelivery: { next: ViewerOfferDelivery };
  /** Записать настройку так, как это делает страница настроек: хранилище + событие. */
  changeOverlaySetting(enabled: boolean): void;
  /** Прогнать часы вперёд и дать отработать промисам. */
  advance(ms: number): Promise<void>;
}

export interface PageUiHarnessOptions {
  readonly overlayEnabled?: boolean;
  readonly isTopFrame?: boolean;
  readonly supportsPopover?: boolean;
  readonly sendFails?: boolean;
  readonly notes?: unknown[];
}

export async function setupPageUi(options: PageUiHarnessOptions = {}): Promise<PageUiHarness> {
  const doc = new FakeUiDocument(options.supportsPopover ?? true);
  const pointer = new FakePointer();
  const clock = createFakeClock();
  const sent: OverlayButtonPressedMessage[] = [];
  const asked: { x: number; y: number }[] = [];
  const answer = { next: (): Promise<VideoAtPoint | null> => Promise.resolve(VIDEO_AT_POINT) };
  const storageListeners = new Set<(changes: Record<string, { newValue?: unknown }>) => void>();
  const signals = new FakeGeometrySignals();
  const returnRequests = { count: 0 };
  const returnDelivery: { next: ViewerReturnDelivery } = { next: "sent" };
  const offerChoices: ViewerOfferChoice[] = [];
  const offerDelivery: { next: ViewerOfferDelivery } = { next: "hand_off" };
  let stored: unknown = options.overlayEnabled === undefined ? undefined : { overlay_button: options.overlayEnabled };

  const ui = createPageUi({
    document: doc,
    viewportSize: () => ({ width: 1280, height: 720 }),
    pointer,
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle as number) },
    now: () => clock.scheduler.now(),
    ownExtensionId: OWN_EXTENSION_ID,
    isTopFrame: options.isTopFrame ?? true,
    videoAtPoint: (x, y) => {
      asked.push({ x, y });
      return answer.next();
    },
    sendPressed: (message) => {
      if (options.sendFails === true) return Promise.reject(new Error("background is gone"));
      sent.push(message);
      return Promise.resolve();
    },
    readSettings: () => Promise.resolve(parseEntrySettings(stored)),
    onLocalStorageChanged: (listener) => {
      storageListeners.add(listener);
      return () => void storageListeners.delete(listener);
    },
    note: (message) => void options.notes?.push(message),
    placeholderSignals: signals,
    requestViewerReturn: () => {
      returnRequests.count += 1;
      return returnDelivery.next;
    },
    answerViewerOffer: (choice) => {
      offerChoices.push(choice);
      return Promise.resolve(offerDelivery.next);
    },
  });
  await flushMicrotasks();

  return {
    ui,
    doc,
    pointer,
    clock,
    sent,
    asked,
    answer,
    storageListeners,
    signals,
    returnRequests,
    returnDelivery,
    offerChoices,
    offerDelivery,
    changeOverlaySetting(enabled) {
      stored = { overlay_button: enabled };
      for (const listener of [...storageListeners]) listener({ [ENTRY_SETTINGS_KEY]: { newValue: stored } });
    },
    async advance(ms) {
      clock.advance(ms);
      await flushMicrotasks();
    },
  };
}
