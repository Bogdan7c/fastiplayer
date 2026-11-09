// Сводка меню вариантов по фреймам вкладки (сессия 14b): чьё меню показывать плееру.
//
// Фреймов с адаптером может быть несколько (плеер сайта в iframe, второй плеер на странице). Правило:
// - главнее фрейм с видео сеанса — меню того плеера, что мы показываем;
// - иначе фрейм с наименьшим номером (стабильно, без мигания между фреймами);
// - донесение старше `VARIANT_REPORT_SILENCE_MS` — фрейм ушёл (iframe удалили, аренда кончилась):
//   фрейм с меню доносит на каждом продлении, раз в секунду.

import type { PageVariantDescription, VariantWatchReport } from "../../shared/variant-watch-contract.ts";

/** Молчание фрейма дольше этого — его меню больше нет (три продления подряд). */
export const VARIANT_REPORT_SILENCE_MS = 3_000;

/** Меню, которое сейчас главное. */
export interface FrameVariantMenu {
  readonly frameId: number;
  /** Паспорт адаптера `id@version`. */
  readonly adapter: string;
  readonly description: PageVariantDescription;
}

export interface VariantFrameSummary {
  /** Донесение фрейма (форма проверена). Пустое описание снимает фрейм сразу. */
  accept(frameId: number, report: VariantWatchReport): void;
  /** Главное меню сейчас; `sessionFrameId` — фрейм видео сеанса, если известен. */
  current(sessionFrameId: number | undefined): FrameVariantMenu | undefined;
}

interface StoredReport {
  readonly adapter: string;
  readonly description: PageVariantDescription;
  readonly receivedAt: number;
}

export function createVariantFrameSummary(now: () => number): VariantFrameSummary {
  const reports = new Map<number, StoredReport>();

  function forgetSilentFrames(): void {
    const currentMs = now();
    for (const [frameId, stored] of reports) {
      if (currentMs - stored.receivedAt >= VARIANT_REPORT_SILENCE_MS) reports.delete(frameId);
    }
  }

  return {
    accept(frameId, report) {
      if (report.description === null) reports.delete(frameId);
      else reports.set(frameId, { adapter: report.adapter, description: report.description, receivedAt: now() });
    },
    current(sessionFrameId) {
      forgetSilentFrames();
      const chosenFrame =
        sessionFrameId !== undefined && reports.has(sessionFrameId) ? sessionFrameId : Math.min(...reports.keys());
      const stored = reports.get(chosenFrame);
      return stored === undefined ? undefined : { frameId: chosenFrame, adapter: stored.adapter, description: stored.description };
    },
  };
}
