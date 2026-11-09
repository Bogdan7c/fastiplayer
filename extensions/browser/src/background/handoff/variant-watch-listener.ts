// Приём донесений фреймов о меню вариантов страницы (сессия 14b) через `runtime.onMessage`. Как у рекламы
// (`ad-watch-listener.ts`): принимаем только свой контент-скрипт, вкладку и фрейм называет браузер, а форма
// проверяется строго ещё раз (описание пришло из мира, где живёт страница).

import type { VariantWatchReport } from "../../shared/variant-watch-contract.ts";
import { parseVariantWatchReport } from "../../shared/variant-watch-validation.ts";
import type { FrameSender } from "./hand-off-command.ts";

export function createVariantWatchReportListener(
  extensionId: string,
  onReport: (tabId: number, frameId: number, report: VariantWatchReport) => void,
): (message: unknown, sender: FrameSender) => boolean {
  return (message, sender) => {
    const tabId = sender.tab?.id;
    if (sender.id !== extensionId || tabId === undefined || sender.frameId === undefined) return false;
    const report = parseVariantWatchReport(message);
    if (report !== undefined) onReport(tabId, sender.frameId, report);
    // Ответа донесение не ждёт: канал держать незачем.
    return false;
  };
}
