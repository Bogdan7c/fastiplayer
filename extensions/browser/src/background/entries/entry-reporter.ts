// Итог входа без попапа: плашка на странице + «!» на значке ЭТОЙ вкладки (решение 211).
//
// Плашка уходит в верхний фрейм вкладки. Её сбой (на странице нет нашего скрипта, вкладка закрыта, страница
// браузера) ничего не роняет: значок всё равно ставится, и человек видит хотя бы его. Сбой значка тоже
// гасится (пишется для разработчика): итог входа не должен ломать сам вход.

import type { EntryReport } from "../../shared/entry-contract.ts";
import { describeEntryResult, type EntryResult } from "./entry-report-text.ts";

/** Показать плашку на вкладке (в проде: `tabs.sendMessage` во фрейм 0). Отклоняется при сбое доставки. */
export interface EntryBanner {
  show(tabId: number, report: EntryReport): Promise<void>;
}

/** «!» на значке вкладки. Глобальный значок (статус связи) эти методы не трогают. */
export interface EntryBadge {
  markProblem(tabId: number): Promise<void>;
  /** Убрать вкладочный значок: вкладка снова наследует глобальный. */
  clearProblem(tabId: number): Promise<void>;
}

export interface EntryReporterDependencies {
  readonly banner: EntryBanner;
  readonly badge: EntryBadge;
  /** Отметка для разработчика о проглоченном сбое (не пользователю). */
  readonly note: (message: string, error: unknown) => void;
}

export interface EntryReporter {
  /** Сообщить итог входа на вкладке. Не отклоняется. */
  report(tabId: number, result: EntryResult): Promise<void>;
  /** Снять вкладочный «!» прошлого отказа (сеанс на вкладке стартовал не через вход, например из попапа). Не отклоняется. */
  forgetProblem(tabId: number): Promise<void>;
}

export function createEntryReporter(dependencies: EntryReporterDependencies): EntryReporter {
  /** Шаг, чей сбой (отказ промиса или исключение) только отмечается для разработчика. */
  async function attempt(step: () => Promise<void>, failureMessage: string): Promise<void> {
    try {
      await step();
    } catch (error) {
      dependencies.note(failureMessage, error);
    }
  }
  return {
    forgetProblem: (tabId) => attempt(() => dependencies.badge.clearProblem(tabId), "entry badge was not cleared"),
    async report(tabId, result) {
      const report = describeEntryResult(result);
      // Плашка и значок независимы: сбой одного не отменяет другого.
      await attempt(() => dependencies.banner.show(tabId, report), "entry banner was not delivered");
      await attempt(
        () => (report.tone === "refusal" ? dependencies.badge.markProblem(tabId) : dependencies.badge.clearProblem(tabId)),
        "entry badge was not updated",
      );
    },
  };
}
