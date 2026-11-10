// Настоящий порт внедрения во фреймы (сессия 15c): `webNavigation.getAllFrames` + `scripting.executeScript`
// в один фрейм без ожидания его загрузки. Логика срока и исходов — в frame-injection.ts.
//
// Право `webNavigation` (решение 299) нужно только для списка фреймов. В Chromium оно не добавляет строки
// в окно установки: его перекрывает уже имеющееся «данные на всех сайтах» (проверено вызовом
// `management.getPermissionWarningsByManifest` в живом Chromium 156). manifest/PERMISSIONS.md.

import { browserApi } from "../../shared/browser-api.ts";
import { createRealScheduler } from "../host-link/native-port.ts";
import { answersOfEachFrame, liveFrameIds, type FrameInjection, type FrameInjectionPort, type SingleFrameResult } from "./frame-injection.ts";

export const browserFrameInjection: FrameInjectionPort = {
  async listFrames(tabId) {
    return liveFrameIds(await browserApi.webNavigation.getAllFrames({ tabId }));
  },
  async injectIntoFrame<Args extends unknown[]>(tabId: number, frameId: number, injection: FrameInjection<Args>): Promise<SingleFrameResult | undefined> {
    const results = await browserApi.scripting.executeScript({
      target: { tabId, frameIds: [frameId] },
      // Не ждать загрузки фрейма (document_idle): ленивый или недогруженный фрейм иначе держал бы ответ вечно.
      injectImmediately: true,
      func: injection.func,
      args: injection.args,
    });
    // Один фрейм — один элемент ответа; Firefox кладёт `undefined` вместо фрейма, куда внедрить нельзя.
    return results[0];
  },
};

/** Таймеры фона для обвязок, у которых нет своего планировщика (поздний вход, возврат страницы). */
const wiringScheduler = createRealScheduler();

/**
 * Ответы всех фреймов вкладки на настоящем браузере — для обвязок позднего входа и возврата страницы.
 * Отказ промиса — читать на вкладке нечего (см. `answersOfEachFrame`).
 */
export function browserAnswersOfEachFrame<Args extends unknown[]>(tabId: number, injection: FrameInjection<Args>, deadlineMs: number): Promise<SingleFrameResult[]> {
  return answersOfEachFrame(tabId, browserFrameInjection, wiringScheduler, injection, deadlineMs);
}
