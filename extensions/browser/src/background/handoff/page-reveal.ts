// «Go to page» (решение 207): вывести вкладку с рекламой и окно браузера вперёд, чтобы человек нажал
// кнопку сайта настоящей мышью. Права не нужны: `tabs.update` без `url` и `windows.update` лежат вне
// списка защищённых свойств, манифест не растёт (проверено по документации Chrome).
//
// Порядок: сначала вкладка (`active`), потом окно (`focused`) — окно нужно именно то, где вкладка.
// Сбой — исключение вызывающему: он запишет его в журнал, а сеанс продолжит жить.

/** Что нужно от браузера. В продукте — `chrome.tabs.update` и `chrome.windows.update`. */
export interface PageRevealApi {
  /** Сделать вкладку активной в её окне; вернуть окно вкладки. */
  activateTab(tabId: number): Promise<{ readonly windowId: number } | undefined>;
  focusWindow(windowId: number): Promise<void>;
}

export function createPageReveal(api: PageRevealApi): (tabId: number) => Promise<void> {
  return async (tabId) => {
    const tab = await api.activateTab(tabId);
    // Вкладки уже нет: браузер отвечает пустотой, а не ошибкой. Фокусировать окно незачем.
    if (tab === undefined) throw new Error("the tab is gone, nothing to reveal");
    await api.focusWindow(tab.windowId);
  };
}
