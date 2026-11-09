// «Показать страницу» (протокол 1.6, решение 278): зритель в окне плеера хочет увидеть страницу, например сменить
// там качество. Браузер переключаем на вкладку сеанса в три шага:
// 1. вкладка становится активной в своём окне (`tabs.update {active}`) — окно при этом НЕ выходит вперёд;
// 2. окно просят выйти вперёд (`windows.update {focused}`);
// 3. окно подсвечивается (`windows.update {drawAttention}`): без эффекта, если фокус уже получен, зато честный запасной
//    ход там, где compositor фокус не отдаёт (KDE Wayland, решение 208) — мигает кнопка окна в панели задач.
// Права не нужны: оба вызова не трогают защищённых свойств (манифест не растёт, проверено по документации Chrome и MDN).
//
// Сбой любого шага — отметка разработчику, а не исключение: сеанс продолжает жить, ему эта просьба не важна.
// Сбой фокуса не отменяет подсветку: она нужна именно тогда, когда фокус не дали.

/** Что нужно от браузера. В продукте — `chrome.tabs.update` и `chrome.windows.update`. */
export interface PageTabRevealApi {
  /** Сделать вкладку активной в её окне; вернуть окно вкладки, а если вкладки уже нет — `undefined`. */
  activateTab(tabId: number): Promise<{ readonly windowId: number } | undefined>;
  /** Попросить окно выйти вперёд (браузер или оконная система вправе отказать, не бросая ошибки). */
  focusWindow(windowId: number): Promise<void>;
  /** Подсветить окно, не меняя фокус; пока человек не переключится на окно. */
  drawAttention(windowId: number): Promise<void>;
}

/** Сеансу нужно одно намерение — «показать вкладку страницы сейчас»; сбои уже разобраны внутри. */
export interface SessionPageReveal {
  /** Показать ТЕКУЩУЮ вкладку страницы (после возврата страницы она другая). Никогда не бросает. */
  show(): Promise<void>;
}

export interface SessionPageRevealOptions {
  readonly api: PageTabRevealApi;
  /** Вкладка страницы сеанса сейчас: спрашивается в момент просьбы, а не при создании. */
  readonly currentTabId: () => number;
  /** Журнал разработчика. */
  readonly note: (message: string) => void;
}

export function createSessionPageReveal(options: SessionPageRevealOptions): SessionPageReveal {
  const { api, currentTabId, note } = options;

  /** Выполнить шаг; отказ уходит в журнал разработчика и дальше не идёт. */
  async function attempt(step: string, tabId: number, action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (error) {
      note(`show_page: ${step} failed for tab ${tabId}: ${String(error)}`);
    }
  }

  return {
    async show() {
      const tabId = currentTabId();
      let windowId: number | undefined;
      try {
        windowId = (await api.activateTab(tabId))?.windowId;
      } catch (error) {
        note(`show_page: activating tab ${tabId} failed: ${String(error)}`);
        return;
      }
      // Вкладки уже нет: браузер отвечает пустотой, а не ошибкой. Окно без вкладки показывать незачем.
      if (windowId === undefined) {
        note(`show_page: tab ${tabId} is gone, nothing to show`);
        return;
      }
      const shownWindow = windowId;
      await attempt("focusing the window", tabId, () => api.focusWindow(shownWindow));
      await attempt("drawing attention to the window", tabId, () => api.drawAttention(shownWindow));
    },
  };
}
