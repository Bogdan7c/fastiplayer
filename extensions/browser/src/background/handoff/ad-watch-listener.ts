// Приём донесений фреймов о рекламе (сессия 09d) через runtime.onMessage. Как и донесение сторожа
// следующего видео: принимаем только свой контент-скрипт, вкладку и фрейм называет браузер, а форма
// проверяется строго ещё раз (числа пришли из мира, где живёт страница).

import type { PageAdState } from "../../protocol/index.ts";
import { parseAdWatchReport } from "../../shared/ad-watch-validation.ts";
import type { FrameSender } from "./hand-off-command.ts";

export function createAdWatchReportListener(
  extensionId: string,
  onReport: (tabId: number, frameId: number, ad: PageAdState) => void,
): (message: unknown, sender: FrameSender) => boolean {
  return (message, sender) => {
    const tabId = sender.tab?.id;
    if (sender.id !== extensionId || tabId === undefined || sender.frameId === undefined) return false;
    const report = parseAdWatchReport(message);
    if (report !== undefined) onReport(tabId, sender.frameId, report.ad);
    // Ответа донесение не ждёт: канал держать незачем.
    return false;
  };
}
