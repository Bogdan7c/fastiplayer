// Что сеанс передачи получает снаружи: связь с посредником, сеть, правила браузера, описание цели.
//
// Вынесено из `handoff-session.ts` без изменения поведения (сессия 10a2): сам сеанс упёрся в предел
// 800 строк, а эти договоры читают и владелец сеансов (`tab-handoffs.ts`), и сборка фона, и тесты.

import type { PageIdentity } from "../../protocol/index.ts";
import type { OpenSessionOutcome, OpenSessionRequest } from "../host-link/open-session-requests.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { ChoiceMemoryStore } from "../choice-memory/choice-memory-store.ts";
import type { AdMirrorBrowser } from "./ad-skip-mirror.ts";
import type { VariantMirrorBrowser } from "./variant-mirror.ts";
import type { FileAddressRule } from "./file-address-guard.ts";
import type { FileReferrerRules } from "./file-referrer-rule.ts";
import type { FrameConnector } from "./frame-port.ts";
import type { LocalAddressBlock } from "./local-address-block-rules.ts";
import type { PageTabRevealApi } from "./page-tab-reveal.ts";
import type { PageReopenBrowser } from "./page-return/page-reopen-browser.ts";
import type { ReturnablePage } from "./page-return/page-return-memory.ts";
import type { RangeFetch } from "./range-fetcher.ts";
import type { AfterPreviousSession } from "./session-contract.ts";
import type { WebSocketFactory } from "./session-socket.ts";
import type { ChosenVideo } from "./video-choice.ts";
import type { WatchTabUpdates } from "./episode-change/session-episode-change.ts";

/** Что сеансу нужно от связи с посредником (HostLink). */
export interface SessionDoor {
  openSession(request: OpenSessionRequest): Promise<OpenSessionOutcome>;
  acquireDemand(reason: string): () => void;
}

export interface HandoffSessionDependencies {
  readonly door: SessionDoor;
  readonly createWebSocket: WebSocketFactory;
  readonly scheduler: Scheduler;
  readonly connectFrame: FrameConnector;
  /** Жива ли вкладка (отличить «закрыли вкладку» от «ушли со страницы»). */
  readonly tabExists: (tabId: number) => Promise<boolean>;
  /** `tabs.get`: заголовок и адрес; `undefined` — вкладки нет. */
  readonly describeTab: (tabId: number) => Promise<TabDescription | undefined>;
  /**
   * Новости о заголовке и адресе вкладок (`tabs.onUpdated`, сессия 16a): сеанс подписывается на время своей
   * жизни и сам выбирает свою вкладку. Ответ — отписка.
   */
  readonly watchTabUpdates: WatchTabUpdates;
  /** Сеть браузера для байтов файла `<video src>` (сессия 09b; в продукте — `fetch` фона). */
  readonly fetchRange: RangeFetch;
  /** Referer закачки файла (решение 202): правило браузера на время одного запроса. */
  readonly fileReferrer: FileReferrerRules;
  /** Сетевой заслон локальных адресов (решение 199): разрешение сеанса на время одного запроса. */
  readonly localAddressBlock: LocalAddressBlock;
  /** Взвести сторож следующего видео всем фреймам вкладки (сессия 09c2). */
  readonly armNextVideo: (tabId: number, leaseMs: number) => void;
  /** Браузерная сторона дублёра кнопки пропуска рекламы: аренда наблюдения, нажатие, вывод вкладки (09d). */
  readonly adMirrorBrowser: AdMirrorBrowser;
  /** Браузерная сторона меню вариантов страницы: аренда наблюдения, команда фрейму (14b). */
  readonly variantMirrorBrowser: VariantMirrorBrowser;
  /** Отметка в журнал разработчика (устаревшая команда плеера, сбой вывода вкладки). */
  readonly noteForDeveloper: (message: string) => void;
  /** Браузерная сторона возврата ушедшей страницы (сессия 11b2): окна, вкладки, взвод сторожа возврата. */
  readonly pageReopen: PageReopenBrowser;
  /** Браузерная сторона «показать страницу» (протокол 1.6, решение 278): вкладка, окно, подсветка. */
  readonly pageTabReveal: PageTabRevealApi;
  /** Память выбора озвучки, качества и субтитров (14b2): одно хранилище фона на все сеансы. */
  readonly choiceMemory: ChoiceMemoryStore;
}

/** Что фон знает о вкладке: подпись окна и адрес документа. */
export interface TabDescription {
  readonly title?: string | undefined;
  readonly url?: string | undefined;
  /** Окно вкладки и его вид: где открыть страницу заново, если она уйдёт (сессия 11b2). */
  readonly windowId?: number | undefined;
  readonly incognito?: boolean | undefined;
}

/** Где живёт страница вкладки — для возврата (11b2); `undefined` — адреса или окна браузер не назвал. */
export function returnablePageOf(tab: TabDescription): ReturnablePage | undefined {
  if (tab.url === undefined || tab.windowId === undefined) return undefined;
  return { url: tab.url, windowId: tab.windowId, incognito: tab.incognito === true };
}

/** Что и куда передаём. */
export interface HandoffTarget {
  readonly tabId: number;
  /** Адрес вкладки при старте: его смена значит, что человек ушёл со страницы. */
  readonly tabUrl: string | undefined;
  readonly frameId: number;
  /** Метка документа, из которого выбран элемент (договор: `begin_handoff.document`). */
  readonly document: string;
  readonly element: number;
  readonly sessionKey: string;
  readonly page: PageIdentity;
  /** Номер запроса `begin_handoff` (уникален в фоне). */
  readonly beginRequestId: number;
  /** Правило адреса файла (решение 199): `file_source` и перенаправления проверяются им же. */
  readonly fileAddressRule: FileAddressRule;
  /** Что передаётся: реклама или фильм, может ли «фильм» оказаться рекламой (09c2), и его длина (11b2). */
  readonly video: Pick<ChosenVideo, "role" | "mayBeAd" | "durationS">;
  /** Где живёт страница: окно и адрес для возврата, если она уйдёт (11b2). */
  readonly pagePlace: ReturnablePage | undefined;
  /** Вкладка в приватном окне (память выбора там не пишет, 14b2); `undefined` — браузер не сказал. */
  readonly incognito: boolean | undefined;
  /**
   * Страница сеанса вернулась в другую вкладку (11b2): владелец сеансов перенаправляет донесения сторожа и
   * закрытие вкладки этому сеансу уже по новому номеру.
   */
  readonly movePageToTab: (tabId: number) => void;
  /** Новый номер запроса `begin_handoff` для переезда. */
  readonly nextBeginRequestId: () => number;
  /** Замена видео на той же вкладке (решение 215): когда начинать страницу; см. `AfterPreviousSession`. */
  readonly afterPreviousSession?: AfterPreviousSession;
}
