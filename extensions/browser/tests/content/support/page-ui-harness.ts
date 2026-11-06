// Стенд для проверки слоя страницы целиком: фейковые DOM, мышь, часы, хранилище и «MAIN-мир».
import type { OverlayButtonPressedMessage, VideoAtPoint } from "../../../src/shared/entry-contract.ts";
import { ENTRY_SETTINGS_KEY, parseEntrySettings } from "../../../src/shared/entry-settings.ts";
import { createPageUi, type PageUi } from "../../../src/content/page-ui/page-ui.ts";
import { createFakeClock, type FakeClock } from "../../background/support.ts";
import { FakePointer, FakeUiDocument, flushMicrotasks } from "./fake-ui-dom.ts";

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
