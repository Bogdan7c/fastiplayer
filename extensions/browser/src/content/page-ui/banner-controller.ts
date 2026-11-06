// Логика плашки с итогом входа: что показано, когда скрыть, чем заменить. DOM здесь нет — рисует `BannerView`.
//
// Правила:
//  - одна плашка; новая заменяет старую и сбрасывает таймер старой (иначе таймер старой погасит новую);
//  - автоскрытие: 5 с для `success`, 8 с для `refusal` (отказ человек должен успеть прочесть), 15 с для `hint`;
//  - «×» убирает плашку сразу.
import type { EntryReport } from "../../shared/entry-contract.ts";

/** Через сколько плашка исчезает сама, мс. Отказ держим дольше: в нём следующий шаг для человека. */
export const BANNER_AUTO_HIDE_MS: Readonly<Record<EntryReport["tone"], number>> = {
  success: 5_000,
  refusal: 8_000,
  // Человеку надо найти кнопку play на странице и нажать её, пока подсказка перед глазами.
  hint: 15_000,
};

/** Отрисовка плашки (в проде — DOM в закрытой тени, в тестах — запись вызовов). */
export interface BannerView {
  /** Показать отчёт вместо прежнего; `onDismissClicked` зовётся при нажатии на «×». */
  show(report: EntryReport, onDismissClicked: () => void): void;
  hide(): void;
}

export interface BannerTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface BannerController {
  show(report: EntryReport): void;
  /** Имя случая показанной плашки; `null` — плашки нет (для зонда стенда). */
  shownId(): string | null;
}

export function createBannerController(view: BannerView, timers: BannerTimers): BannerController {
  let shown: EntryReport | null = null;
  let hideTimer: unknown = null;

  function cancelHideTimer(): void {
    if (hideTimer === null) return;
    timers.clearTimeout(hideTimer);
    hideTimer = null;
  }

  function dismiss(): void {
    cancelHideTimer();
    shown = null;
    view.hide();
  }

  return {
    show(report) {
      cancelHideTimer();
      shown = report;
      // Замыкание держит САМУ эту плашку: «×» старой, нажатый в гонке с новой, не должен убрать новую.
      view.show(report, () => {
        if (shown === report) dismiss();
      });
      hideTimer = timers.setTimeout(() => {
        hideTimer = null;
        if (shown === report) dismiss();
      }, BANNER_AUTO_HIDE_MS[report.tone]);
    },
    shownId: () => shown?.id ?? null,
  };
}
