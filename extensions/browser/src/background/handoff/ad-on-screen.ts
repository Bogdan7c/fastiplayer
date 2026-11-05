// Сводка «реклама на экране» (сессия 09d): владелец того, что фон говорит плееру в `ad_state`.
//
// Входы:
// - донесения фреймов (`reportFromFrame`): органы рекламы читает сам фрейм в своём изолированном мире;
// - признак сеанса `relocation.adOnScreen()` (решение 204): «играет ролик, который мы приняли за рекламу».
//
// Правило сводки:
// 1. есть живое донесение `showing` — оно (если их несколько, берём самое полезное зрителю);
// 2. иначе признак сеанса — `showing{pod:unknown, skip:unavailable}`: «реклама есть, органов не знаем»;
// 3. иначе `none`.
//
// Донесение живёт `AD_REPORT_SILENCE_MS` после последнего: пока реклама идёт, фрейм повторяет его
// на каждом продлении аренды, а фрейм IMA просто удаляют, сказать «конец» ему некому. Молчание — конец.
//
// Наружу уходят только ИЗМЕНЕНИЯ, без дублей. Признак сеанса меняется при переезде на следующий ролик,
// сводка о нём не узнает сама, поэтому владелец зовёт `recompute()` при каждом его возможном изменении.
// Числа в донесениях уже проверены строгим разбором (`parseAdWatchReport`); сводка им верит.

import type { PageAdState } from "../../protocol/index.ts";

/** Сколько живёт донесение фрейма без повторения. Продление идёт раз в секунду: три потеряться могут. */
export const AD_REPORT_SILENCE_MS = 3_000;

/** Состояние «реклама есть, но органов мы не знаем» (рекламу узнали по самому ролику). */
const AD_WITHOUT_ORGANS: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "unavailable" } };

const NO_AD: PageAdState = { kind: "none" };

type ShowingAd = Extract<PageAdState, { kind: "showing" }>;

interface FrameReport {
  readonly ad: ShowingAd;
  readonly receivedAtMs: number;
}

export interface AdOnScreenSummaryDependencies {
  /** Монотонные миллисекунды. */
  readonly now: () => number;
  /** Признак сеанса: идёт ролик, принятый за рекламу. */
  readonly sessionSaysAdOnScreen: () => boolean;
  /** Сводка изменилась: новое состояние уходит плееру. Первое состояние — `none`, его не объявляют. */
  readonly onChange: (ad: PageAdState) => void;
}

export interface AdOnScreenSummary {
  /** Донесение фрейма; `none` снимает прежнее донесение этого фрейма. */
  reportFromFrame(frameId: number, ad: PageAdState): void;
  /** Пересчитать: признак сеанса мог измениться, донесения — устареть. */
  recompute(): void;
  /** Текущая сводка. */
  current(): PageAdState;
  /** Фрейм, который сказал `available` и на который идёт сводка; только ему можно слать «нажми». */
  frameWithAvailableSkip(): number | undefined;
}

/** Чем полезнее состояние зрителю: нажимаемая кнопка лучше «нажмёт человек», та лучше отсчёта. */
function usefulness(ad: ShowingAd): number {
  const skipRank = { available: 4, on_page: 3, countdown: 2, unavailable: 1 }[ad.skip.kind];
  return skipRank * 2 + (ad.pod.kind === "known" ? 1 : 0);
}

export function createAdOnScreenSummary(dependencies: AdOnScreenSummaryDependencies): AdOnScreenSummary {
  const reports = new Map<number, FrameReport>();
  let announcedKey = JSON.stringify(NO_AD);
  let announced: PageAdState = NO_AD;

  function dropStaleReports(): void {
    const deadline = dependencies.now() - AD_REPORT_SILENCE_MS;
    for (const [frameId, report] of reports) {
      if (report.receivedAtMs <= deadline) reports.delete(frameId);
    }
  }

  /** Лучшее донесение: самое полезное, при равенстве — фрейм с меньшим номером (порядок стабилен). */
  function bestReport(): { readonly frameId: number; readonly ad: ShowingAd } | undefined {
    let best: { frameId: number; ad: ShowingAd } | undefined;
    for (const [frameId, report] of reports) {
      const better =
        best === undefined ||
        usefulness(report.ad) > usefulness(best.ad) ||
        (usefulness(report.ad) === usefulness(best.ad) && frameId < best.frameId);
      if (better) best = { frameId, ad: report.ad };
    }
    return best;
  }

  function summarize(): PageAdState {
    const report = bestReport();
    if (report !== undefined) return report.ad;
    return dependencies.sessionSaysAdOnScreen() ? AD_WITHOUT_ORGANS : NO_AD;
  }

  function recompute(): void {
    dropStaleReports();
    const summary = summarize();
    const key = JSON.stringify(summary);
    if (key === announcedKey) return;
    announcedKey = key;
    announced = summary;
    dependencies.onChange(summary);
  }

  return {
    reportFromFrame(frameId, ad) {
      if (ad.kind === "none") reports.delete(frameId);
      else reports.set(frameId, { ad, receivedAtMs: dependencies.now() });
      recompute();
    },
    recompute,
    current: () => announced,
    frameWithAvailableSkip(): number | undefined {
      dropStaleReports();
      const report = bestReport();
      return report?.ad.skip.kind === "available" ? report.frameId : undefined;
    },
  };
}
