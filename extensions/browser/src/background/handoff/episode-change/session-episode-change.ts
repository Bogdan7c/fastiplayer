// Смена серии на той же странице для одного сеанса передачи (сессия 16a, решения 303, 306–308, 42).
//
// Связывает три правила с браузером и сеансом:
// - заголовок вкладки → `page_title_changed` (page-title-relay.ts);
// - адрес вкладки с начала текущего видео → «одностраничник ушёл» (page-address-tracker.ts);
// - начало ожидания следующей серии → `awaiting_next_episode` один раз на видео (next-episode-notice.ts).
//
// Источник новостей о вкладке — подписка `watchTabUpdates` (в продукте — `tabs.onUpdated`,
// handoff/browser-wiring.ts). Подписка живёт ровно столько, сколько сеанс: `stop()` её снимает. Вкладка
// сравнивается В МИГ СОБЫТИЯ (`currentTabId`): после возврата страницы (11b2) сеанс живёт в другой вкладке,
// и заголовки старой к нему не относятся. Фазой, сокетом и вкладкой по-прежнему владеет сеанс: сюда они
// приходят только узкими вопросами.

import type { PageSessionMessage, ProtocolVersion } from "../../../protocol/index.ts";
import { createNextEpisodeNotice, type NextEpisodeNoticeOutcome } from "./next-episode-notice.ts";
import { createPageAddressTracker, type PageAddressSinceVideoBegan } from "./page-address-tracker.ts";
import { createPageTitleRelay } from "./page-title-relay.ts";

/** Что браузер сообщил о вкладке: новый заголовок и/или новый адрес главного документа (`undefined` — не менялся). */
export interface TabUpdate {
  readonly tabId: number;
  readonly title: string | undefined;
  readonly url: string | undefined;
}

/** Подписка на новости о вкладках браузера; ответ — отписка. */
export type WatchTabUpdates = (listener: (update: TabUpdate) => void) => () => void;

export interface SessionEpisodeChangePorts {
  readonly watchTabUpdates: WatchTabUpdates;
  /** Вкладка страницы сеанса сейчас (после возврата страницы — другая). */
  readonly currentTabId: () => number;
  /** Что ушло плееру при открытии сеанса: очищенный заголовок и адрес вкладки при старте. */
  readonly openedPage: { readonly title: string; readonly url: string | undefined };
  readonly agreedProtocol: () => ProtocolVersion;
  /** Слушает ли плеер сейчас слова страницы (связь принята, страница на месте и не возвращается). */
  readonly playerHearsPage: () => boolean;
  readonly send: (message: PageSessionMessage) => void;
}

export interface SessionEpisodeChange {
  /** Тень встала на видео или страница начала новое поколение: новая серия на экране плеера. */
  videoBegan(): void;
  /** Началось ожидание следующей серии (тень на том же элементе или переезд сеанса). */
  nextEpisodeWaitBegan(): NextEpisodeNoticeOutcome;
  /** Сменился ли адрес вкладки с начала текущего видео (одностраничник ушёл, решение 42). */
  pageAddressSinceVideoBegan(): PageAddressSinceVideoBegan;
  /** Сеанс кончился: снять подписку на вкладки. Повторный вызов ничего не делает. */
  stop(): void;
}

export function createSessionEpisodeChange(ports: SessionEpisodeChangePorts): SessionEpisodeChange {
  const sayToPlayer = { agreedProtocol: ports.agreedProtocol, playerHearsPage: ports.playerHearsPage, send: ports.send };
  const titleRelay = createPageTitleRelay({ ...sayToPlayer, openedTitle: ports.openedPage.title });
  const notice = createNextEpisodeNotice(sayToPlayer);
  const address = createPageAddressTracker(ports.openedPage.url);
  let unsubscribe: (() => void) | undefined = ports.watchTabUpdates((update) => {
    if (update.tabId !== ports.currentTabId()) return;
    if (update.url !== undefined) address.addressSeen(update.url);
    if (update.title !== undefined) titleRelay.titleSeen(update.title);
  });

  return {
    videoBegan() {
      address.videoBegan();
      notice.videoBegan();
    },
    nextEpisodeWaitBegan: () => notice.waitBegan(),
    pageAddressSinceVideoBegan: () => address.sinceVideoBegan(),
    stop() {
      unsubscribe?.();
      unsubscribe = undefined;
    },
  };
}
