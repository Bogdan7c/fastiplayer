// Подделка браузерной стороны возврата страницы (сессия 11b2): окна, вкладки, взвод сторожа, видео вкладки.
// Ведёт журнал всего, что фон попросил у браузера, и даёт сценарию решать, что браузер отвечает.
import type { PageReopenBrowser, PageReturnArmOutcome, ReopenedTab, ReturnPageVideo } from "../../../src/background/handoff/page-return/page-reopen-browser.ts";
import type { ReturnablePage } from "../../../src/background/handoff/page-return/page-return-memory.ts";
import type { PageReturnTarget } from "../../../src/shared/handoff-contract.ts";

/** Номер вкладки, которую подделка открывает. */
export const REOPENED_TAB = 31;

/** Окно, которое подделка считает живым по умолчанию. */
export const BROWSER_WINDOW = 5;

/** Ручки сценария: что отвечает «браузер». */
export interface FakeReopenBehavior {
  /** Окно для возврата (`undefined` — окна нужного вида нет). */
  window: number | undefined;
  /** Уже открытая вкладка той же страницы (`undefined` — нет). */
  openTab: number | undefined;
  /** Была ли уже открытая вкладка заглушена. */
  openTabWasMuted: boolean;
  /** Ответ взвода: скрипт в новой вкладке есть или ещё нет. */
  arm: PageReturnArmOutcome;
  /** Видео вкладки при отказе по сроку. */
  videos: readonly ReturnPageVideo[];
  /** Открытие вкладки отвергается (`tabs.create` бросил). */
  openFails: boolean;
  /** Открытие вкладки «висит», пока сценарий не позовёт `finishOpening`. */
  openHangs: boolean;
}

export function createFakePageReopenBrowser(page: ReturnablePage) {
  const behavior: FakeReopenBehavior = {
    window: BROWSER_WINDOW,
    openTab: undefined,
    openTabWasMuted: false,
    arm: "armed",
    videos: [],
    openFails: false,
    openHangs: false,
  };
  const opened: { readonly windowId: number; readonly url: string }[] = [];
  const broughtForward: number[] = [];
  const closed: number[] = [];
  const muted: { readonly tabId: number; readonly muted: boolean }[] = [];
  const arms: { readonly tabId: number; readonly leaseMs: number; readonly target: PageReturnTarget }[] = [];
  const windowQuestions: ReturnablePage[] = [];
  let finishOpening: () => void = () => {};

  const browser: PageReopenBrowser = {
    findWindow: async (asked) => {
      windowQuestions.push(asked);
      return behavior.window;
    },
    findOpenTab: async () => behavior.openTab,
    openTab: (windowId, url) => {
      opened.push({ windowId, url });
      if (behavior.openFails) return Promise.reject(new Error("test: tabs.create failed"));
      const tab: ReopenedTab = { tabId: REOPENED_TAB, openedBy: "page_return", wasMuted: false };
      if (!behavior.openHangs) return Promise.resolve(tab);
      return new Promise((resolve) => (finishOpening = () => resolve(tab)));
    },
    bringForwardMuted: async (tabId) => {
      broughtForward.push(tabId);
      return { tabId, openedBy: "viewer", wasMuted: behavior.openTabWasMuted };
    },
    setMuted: async (tabId, isMuted) => void muted.push({ tabId, muted: isMuted }),
    closeTab: async (tabId) => void closed.push(tabId),
    armPageReturn: async (tabId, leaseMs, target) => {
      arms.push({ tabId, leaseMs, target });
      return behavior.arm;
    },
    describeTab: async (tabId) => ({ ...page, windowId: tabId === REOPENED_TAB ? BROWSER_WINDOW : page.windowId }),
    readVideos: async () => behavior.videos,
  };
  return {
    browser,
    behavior,
    opened,
    broughtForward,
    closed,
    muted,
    arms,
    windowQuestions,
    finishOpening: () => finishOpening(),
  };
}

export type FakePageReopen = ReturnType<typeof createFakePageReopenBrowser>;
