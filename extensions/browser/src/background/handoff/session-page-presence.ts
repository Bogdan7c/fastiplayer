// Присутствие страницы сеанса передачи: в какой вкладке она живёт и почему пропал её порт.
//
// Вынесено из `handoff-session.ts` (упёрся в предел размера) в сессии 11b2, когда вкладка страницы перестала
// быть постоянной: после возврата страница живёт в другой вкладке (решения 240–241). Поведение вопросов
// браузеру прежнее (09a, 09c2):
// - порт главного документа умер раньше `tabs.onRemoved` — спросить, жива ли вкладка: жива — ушли со страницы
//   (`navigated_away`), нет — закрыли (`closed`);
// - порт рекламного фрейма умер — спросить адрес вкладки: другой адрес — ушли, вкладки нет — закрыли, тот же —
//   страница на месте, убрали только ролик.
// Вопрос задаётся один за раз: плеер шлёт часы каждые 250 мс, и каждая потерянная команда спросила бы снова.

import type { TabGoneReason } from "../../protocol/index.ts";
import type { TabDescription } from "./handoff-session-dependencies.ts";

/** Ответ на вопрос «страница на месте?» после смерти порта рекламного фрейма. */
export type FrameLossAnswer = "same_page" | Extract<TabGoneReason, "closed" | "navigated_away">;

/** Что модулю нужно от браузера. */
export interface PagePresenceBrowser {
  readonly tabExists: (tabId: number) => Promise<boolean>;
  readonly describeTab: (tabId: number) => Promise<TabDescription | undefined>;
}

export interface PagePresence {
  /** Вкладка страницы сейчас. */
  tabId(): number;
  /** Страница переехала в другую вкладку (возврат, 11b2): прежняя отметка «закрыта» к новой не относится. */
  moveTo(tabId: number, tabUrl: string | undefined): void;
  /** Браузер сообщил о закрытии вкладки (`tabs.onRemoved`). */
  markRemoved(): void;
  wasRemoved(): boolean;
  /** Порт главного документа умер: закрыли вкладку или ушли со страницы. Пока вопрос в пути — новый не задаётся. */
  askWhyPageLeft(onAnswer: (reason: TabGoneReason) => void): void;
  /** Порт рекламного фрейма умер: страница та же, или её закрыли, или ушли. */
  askWhetherPageStayed(onAnswer: (answer: FrameLossAnswer) => void): void;
}

export function createPagePresence(start: { readonly tabId: number; readonly tabUrl: string | undefined }, browser: PagePresenceBrowser): PagePresence {
  let tabId = start.tabId;
  let tabUrl = start.tabUrl;
  let removed = false;
  let asking = false;

  /** Один вопрос за раз; ответ приходит, только если вкладка за это время не сменилась. */
  function ask<T>(question: (askedTabId: number) => Promise<T>, onAnswer: (value: T) => void, onFailure: () => void): void {
    if (asking) return;
    asking = true;
    const askedTabId = tabId;
    const settle = (deliver: () => void): void => {
      asking = false;
      if (askedTabId === tabId) deliver();
    };
    question(askedTabId).then(
      (value) => settle(() => onAnswer(value)),
      () => settle(onFailure),
    );
  }

  return {
    tabId: () => tabId,
    moveTo(nextTabId, nextTabUrl) {
      tabId = nextTabId;
      tabUrl = nextTabUrl;
      removed = false;
      asking = false;
    },
    markRemoved() {
      removed = true;
    },
    wasRemoved: () => removed,
    askWhyPageLeft(onAnswer) {
      ask(
        (asked) => browser.tabExists(asked),
        (exists) => onAnswer(exists && !removed ? "navigated_away" : "closed"),
        () => onAnswer("closed"),
      );
    },
    askWhetherPageStayed(onAnswer) {
      ask(
        (asked) => browser.describeTab(asked),
        (tab) => {
          if (tab === undefined) onAnswer("closed");
          else onAnswer(tab.url === tabUrl ? "same_page" : "navigated_away");
        },
        () => onAnswer("closed"),
      );
    },
  };
}
