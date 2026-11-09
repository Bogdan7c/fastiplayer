// Браузерная сторона «показать страницу» (протокол 1.6, решение 278) на настоящих API (chrome.* есть и в Firefox MV3).
// Логика и порядок шагов — в page-tab-reveal.ts; здесь только переходники.
//
// Прав манифеста не добавляет: `tabs.update` без `url` и `windows.update` с `focused`/`drawAttention` лежат вне
// списка защищённых свойств (Chrome, MDN). `drawAttention` не действует, если окно уже в фокусе.

import { browserApi } from "../../shared/browser-api.ts";
import type { PageTabRevealApi } from "./page-tab-reveal.ts";

export const browserPageTabReveal: PageTabRevealApi = {
  async activateTab(tabId) {
    // Вкладки нет: Chrome отвечает ошибкой, Firefox — тоже; сеанс запишет её в журнал разработчика.
    const tab = await browserApi.tabs.update(tabId, { active: true });
    return tab === undefined ? undefined : { windowId: tab.windowId };
  },
  async focusWindow(windowId) {
    await browserApi.windows.update(windowId, { focused: true });
  },
  async drawAttention(windowId) {
    await browserApi.windows.update(windowId, { drawAttention: true });
  },
};
