// Что открытию страницы нужно от браузера (сессия 11b2). Только договор: настоящая сборка — в
// `browser-wiring.ts`, в тестах — подделка. Так правила открытия проверяются без браузера.
//
// Факты разведки 11b2 (Chromium 156, Firefox 158, KDE Wayland), на которых стоит договор:
// - видео запускается во вкладке, АКТИВНОЙ в своём окне, даже когда окно браузера позади другого окна;
//   в фоновой вкладке браузеры видео не грузят (Chromium) или не запускают (Firefox);
// - Chromium поднимает окно браузера поверх всех, если вкладку создать сразу активной (`tabs.create` с
//   `active: true`), и не поднимает, если создать её в фоне и потом сделать активной (`tabs.update`);
// - новое окно (`windows.create`, даже `focused: false`) всегда встаёт поверх окна плеера — поэтому
//   нового окна возврат не открывает: нет окна нужного вида — честный отказ `no_browser_window`.

import type { PageReturnTarget } from "../../../shared/handoff-contract.ts";
import type { ReturnablePage } from "./page-return-memory.ts";

/** Что сказал взвод сторожа в новой вкладке. */
export type PageReturnArmOutcome =
  /** Наш скрипт в документе есть и взведён. */
  | "armed"
  /** Получателя нет: страница ещё грузится, страница ошибки или браузер скрипт не вставил. */
  | "no_script";

/** Видео вкладки, как его видит фон при отказе (длина — для причины «не то видео»). */
export interface ReturnPageVideo {
  /** `null` — длина неизвестна или бесконечна (эфир). */
  readonly durationS: number | null;
}

/** Вкладка, в которой ждём страницу. */
export interface ReopenedTab {
  readonly tabId: number;
  /** Открыта нами (при отказе закрываем) или уже была открыта зрителем (оставляем). */
  readonly openedBy: "page_return" | "viewer";
  /** Была ли вкладка заглушена до нас: после возврата звук вкладки возвращается как был. */
  readonly wasMuted: boolean;
}

export interface PageReopenBrowser {
  /** Окно для возврата: прежнее, если живо и того же вида (инкогнито или нет), иначе любое обычное окно того же вида. */
  findWindow(page: ReturnablePage): Promise<number | undefined>;
  /** Уже открытая вкладка с тем же адресом в окне того же вида (`undefined` — такой нет). */
  findOpenTab(page: ReturnablePage): Promise<number | undefined>;
  /** Новая вкладка: создать в фоне, заглушить, сделать активной (окно браузера не всплывает, см. шапку). */
  openTab(windowId: number, url: string): Promise<ReopenedTab>;
  /** Уже открытую вкладку — заглушить и сделать активной. */
  bringForwardMuted(tabId: number): Promise<ReopenedTab>;
  /** Звук вкладки: после возврата — как был, при отказе — тоже. */
  setMuted(tabId: number, muted: boolean): Promise<void>;
  closeTab(tabId: number): Promise<void>;
  /** Взвести сторож возврата во всех фреймах вкладки. */
  armPageReturn(tabId: number, leaseMs: number, target: PageReturnTarget): Promise<PageReturnArmOutcome>;
  /** Где теперь живёт страница (адрес, окно, инкогнито); `undefined` — вкладки нет. */
  describeTab(tabId: number): Promise<ReturnablePage | undefined>;
  /** Видео всех фреймов вкладки; сбой чтения — отказ промиса. */
  readVideos(tabId: number): Promise<readonly ReturnPageVideo[]>;
}
