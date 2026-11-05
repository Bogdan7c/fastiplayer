// Жизнь вкладок браузера глазами модели (сессия 09c, решение 198): подмена вкладки и уборка на старте
// фона. `tabs.onRemoved` подключён там же, где и передача (handoff/browser-wiring.ts): закрытие касается их обоих.

import type { TabModel } from "./tab-model.ts";

/** Нужная модели часть `chrome.tabs`. */
export interface TabsLifecycleApi {
  readonly onReplaced: { addListener(listener: (addedTabId: number, removedTabId: number) => void): void };
  query(queryInfo: Record<string, never>): Promise<readonly { readonly id?: number | undefined }[]>;
}

/**
 * Подписать модель на браузер. Список вкладок нужен потому, что `tabs.onRemoved` мог потеряться (фон
 * упал посреди записи, сбой хранилища), а запись в `storage.session` живёт до конца сеанса браузера.
 * Список не получен — не стираем ничего: лучше лишняя запись, чем снимок живой вкладки.
 */
export function followTabLifecycle(model: Pick<TabModel, "moveTab" | "dropClosedTabs">, tabs: TabsLifecycleApi, reportListFailure: (error: unknown) => void): Promise<void> {
  tabs.onReplaced.addListener((addedTabId, removedTabId) => void model.moveTab(removedTabId, addedTabId));
  return tabs.query({}).then(
    (openTabs) => model.dropClosedTabs(openTabs.flatMap((tab) => (tab.id === undefined ? [] : [tab.id]))),
    reportListFailure,
  );
}
