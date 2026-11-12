// Режим 2 «автозапуск» (сессия 12a): фон судит, передавать ли видео, которое человек сам запустил со звуком.
//
// Страница (MAIN-мир) лишь честно сообщает «сторож задержал видео, потому что человек запустил его со
// звуком» (`trigger: "viewer_play"`). Правил сайтов, стоп-листа и порога она не знает, судит один фон.
//
// Роль модуля: маршрутизатор донесений сторожа.
// - `next_video` (переезд сеанса 09c2, возврат страницы 11b2) идёт в `handoffs.heldVideo` без изменений;
// - `viewer_play`:
//   1. у вкладки уже есть передача -> сначала её собственный сторож (`handoffs.heldVideo`): переезд сеанса
//      (реклама кончилась, 09c2) и сеанс-замена после перезагрузки страницы (16c) главнее. Сеанс не взял видео — человек сам запустил ДРУГОЕ видео той же вкладки:
//      оно судится как новое и при «передать» заменяет прежнее в том же окне (решения 40, 215: последнее
//      заменяет; иначе звучали бы два видео сразу). Замену ведёт `handOffTab` с выбранным видео;
//   2. на вкладке уже идёт решение режима 2 -> `release` (одно решение на вкладку);
//   3. иначе: настройки (каждый раз заново) -> адрес вкладки -> политика сайта -> классификация -> решение
//      (`decideViewerPlay`), при короткой записи — проба рекламы и повторное решение;
//   4. `hand_off` -> ответ `adopt_new_session`, и без ожидания обычная передача `handOffTab` с точной ссылкой на
//      видео; `begin_handoff` нового сеанса сам заберёт тень. Если передача не `started` — фрейму уходит
//      просьба отпустить видео, а человек получает плашку по `noticeOfAutoHandoffOutcome`;
//   5. `release_with_credit` -> всем фреймам вкладки «кредит клика», ответ `release`;
//   6. `ask_viewer` (сайт `ask`, сессия 12b) -> ответ `ask_viewer`: видео замирает под тенью, на нём предложение.
//      Вкладка сразу свободна для других решений; что делать с выбором человека, решает `viewer-offers.ts`, а
//      передачу по выбору «в плеер» ведёт тот же `startHandoff`, что и у обычного решения.
//
// Всё, что касается браузера, приходит снаружи (`AutoHandoffDependencies`), поэтому модуль проверяется на подделках.

import type { HeldVideoReply, HeldVideoReport, VideoRef } from "../../shared/handoff-contract.ts";
import type { ViewerOfferAnswer, ViewerOfferChoiceMessage } from "../../shared/viewer-offer-contract.ts";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../shared/auto-handoff-settings.ts";
import type { EntryResult } from "../entries/entry-report-text.ts";
import type { HeldVideoAnswer } from "../handoff/hand-off-command.ts";
import type { TabHandoffs } from "../handoff/tab-handoffs.ts";
import { classifySurvey, type ClassifiedVideo } from "../handoff/video-choice.ts";
import type { AdProbe } from "./ad-probe.ts";
import { noticeOfAutoHandoffOutcome } from "./outcome-notice.ts";
import { sitePolicyOf, type SitePolicy } from "./site-policy.ts";
import { decideViewerPlay, type AdProbeVerdict, type ViewerPlayDecision } from "./viewer-play-decision.ts";
import { candidatesAsFiltersSee, videoFiltersFor, type VideoFilters } from "./video-filters.ts";
import { createViewerOffers } from "./viewer-offers.ts";

export interface AutoHandoffDependencies {
  /** Шов передачи вкладки: только то, что нужно режиму 2. */
  readonly handoffs: Pick<TabHandoffs, "heldVideo" | "handOffTab" | "activeTabIds">;
  /** Свежая настройка из хранилища. Может отклониться: тогда режим считается выключенным. */
  readonly readSettings: () => Promise<AutoHandoffSettings>;
  /** Записать настройку целиком (правило сайта с предложения «Ask»). Может отклониться. */
  readonly writeSettings: (settings: AutoHandoffSettings) => Promise<void>;
  /** Адрес вкладки; `undefined` — вкладки уже нет или адрес не отдан. */
  readonly describeTab: (tabId: number) => Promise<{ readonly url?: string | undefined } | undefined>;
  readonly adProbe: Pick<AdProbe, "probe">;
  /** «Кредит клика» всем фреймам вкладки. Сбой доставки не бросает (фрейм без скрипта — обычное дело). */
  readonly grantPlayCredit: (tabId: number, leaseMs: number) => void;
  /** Просьба одному фрейму отпустить задержанное видео (передача не состоялась). */
  readonly releaseHeldVideo: (tabId: number, frameId: number, element: number) => void;
  /** Просьба одному фрейму снять предложение «Ask» (видео остаётся на паузе). Сбой доставки не бросает. */
  readonly withdrawViewerOffer: (tabId: number, frameId: number, element: number) => void;
  /** Плашка и значок вкладки. Не отклоняется. */
  readonly reporter: { report(tabId: number, result: EntryResult): Promise<void> };
  /** Отметка для разработчика о сбое, который не должен пропасть молча. */
  readonly note: (message: string, error: unknown) => void;
}

export interface AutoHandoff {
  /** Донесение сторожа; ответ готов сразу или придёт позже. Не отклоняется. */
  heldVideo(tabId: number, frameId: number, report: HeldVideoReport): HeldVideoAnswer;
  /** Выбор человека на предложении «Ask» (сессия 12b). Не отклоняется. */
  viewerOfferChoice(tabId: number, frameId: number, message: ViewerOfferChoiceMessage): Promise<ViewerOfferAnswer>;
  /** Вкладка закрыта: забыть её предложение. */
  tabRemoved(tabId: number): void;
}

export function createAutoHandoff(dependencies: AutoHandoffDependencies): AutoHandoff {
  // Вкладки, на которых режим 2 сейчас решает или стартует передачу. Один заход на вкладку.
  const decisionsInFlight = new Set<number>();

  /** Настройка; сбой чтения — значения по умолчанию (режим выключен), причина в журнал. */
  async function currentSettings(): Promise<AutoHandoffSettings> {
    try {
      return await dependencies.readSettings();
    } catch (error) {
      dependencies.note("could not read the auto handoff settings, mode 2 stays off", error);
      return DEFAULT_AUTO_HANDOFF_SETTINGS;
    }
  }

  /** Адрес вкладки (`""` — неизвестен: политика пропустит сайт, списки фильтров не сработают). */
  async function tabUrlOf(tabId: number): Promise<string> {
    return (await dependencies.describeTab(tabId))?.url ?? "";
  }

  /**
   * Задержанное видео среди кандидатов донесения, с вердиктом общего классификатора. Кандидаты сначала
   * проходят через фильтры видео (`candidatesAsFiltersSee`): где фильтр звука не действует, классификатор
   * не спишет беззвучное видео в мусор.
   */
  function heldVideoOf(frameId: number, report: HeldVideoReport, filters: VideoFilters): ClassifiedVideo | undefined {
    const candidates = candidatesAsFiltersSee(report.candidates, report.element, filters);
    return classifySurvey([{ frameId, candidates }]).find(({ candidate }) => candidate.element === report.element);
  }

  /** Решение; короткий ролик без опознанной рекламы идёт через пробу и второе решение. */
  async function decide(policy: SitePolicy, held: ClassifiedVideo | undefined, filters: VideoFilters, tabId: number): Promise<ViewerPlayDecision> {
    const first = decideViewerPlay({ policy, held, filters, adProbe: "not_probed" });
    if (first.kind !== "probe_ads_first") return first;
    const adProbe: AdProbeVerdict = await dependencies.adProbe.probe(tabId);
    return decideViewerPlay({ policy, held, filters, adProbe });
  }

  /** Начать обычную передачу и не ждать её: фрейм получит `begin_handoff` после запуска плеера. */
  function startHandoff(tabId: number, frameId: number, ref: VideoRef): void {
    const finish = (outcome: Awaited<ReturnType<TabHandoffs["handOffTab"]>>): void => {
      decisionsInFlight.delete(tabId);
      if (outcome.kind !== "started") dependencies.releaseHeldVideo(tabId, frameId, ref.element);
      const notice = noticeOfAutoHandoffOutcome(outcome);
      if (notice !== undefined) void dependencies.reporter.report(tabId, notice);
    };
    // Команда передачи не отклоняется по договору, но отказ тоже не должен оставить видео под тенью.
    dependencies.handoffs.handOffTab(tabId, undefined, ref).then(finish, (error: unknown) => {
      dependencies.note("the auto handoff command threw", error);
      finish({ kind: "failed", reason: `internal:${String(error)}` });
    });
  }

  const viewerOffers = createViewerOffers({
    readSettings: currentSettings,
    writeSettings: dependencies.writeSettings,
    describeTab: dependencies.describeTab,
    // Выбор «в плеер» держит вкладку так же, как обычное решение: до исхода передачи (`startHandoff`).
    startHandoff: (tabId, frameId, ref) => {
      decisionsInFlight.add(tabId);
      startHandoff(tabId, frameId, ref);
    },
    withdrawOffer: dependencies.withdrawViewerOffer,
    note: dependencies.note,
  });

  /** Заход `viewer_play`, который живой сеанс вкладки (если он есть) не взял. Вкладка уже в `decisionsInFlight`. */
  async function judgeViewerPlay(tabId: number, frameId: number, report: HeldVideoReport): Promise<HeldVideoReply> {
    const settings = await currentSettings();
    const tabUrl = await tabUrlOf(tabId);
    const policy = sitePolicyOf(tabUrl, settings);
    const filters = videoFiltersFor(settings, tabUrl);
    const held = heldVideoOf(frameId, report, filters);
    const decision = await decide(policy, held, filters, tabId);
    switch (decision.kind) {
      case "hand_off": {
        if (held === undefined) return "release";
        // Живая передача вкладки (или начатая другим входом, пока судили) не мешает: `handOffTab` с выбранным
        // видео сам решит — то же видео уже в плеере (`started`) или замена в том же окне (решение 215).
        startHandoff(tabId, frameId, { frame_id: held.frameId, document: held.candidate.document, element: held.candidate.element });
        return "adopt_new_session";
      }
      case "ask_viewer":
        if (held === undefined) return "release";
        viewerOffers.offerStood(tabId, { frame_id: held.frameId, document: held.candidate.document, element: held.candidate.element });
        return "ask_viewer";
      case "release_with_credit":
        dependencies.grantPlayCredit(tabId, decision.lease_ms);
        return "release";
      case "probe_ads_first":
      case "release":
        return "release";
    }
  }

  function viewerPlay(tabId: number, frameId: number, report: HeldVideoReport): HeldVideoAnswer {
    if (decisionsInFlight.has(tabId)) return "release";
    // Переезд живого сеанса на это видео (09c2) или сеанс-замена после перезагрузки страницы (16c) главнее:
    // сеанс взял видео — больше решать нечего, второе решение открыло бы вторую передачу того же видео.
    if (dependencies.handoffs.activeTabIds().includes(tabId)) {
      const sessionReply = dependencies.handoffs.heldVideo(tabId, frameId, report);
      if (sessionReply === "adopt" || sessionReply === "adopt_new_session") return sessionReply;
    }
    decisionsInFlight.add(tabId);
    return judgeViewerPlay(tabId, frameId, report).then(
      (reply) => {
        // Передача, которую мы начали, держит вкладку до своего исхода (`startHandoff`); остальным заходам вкладка свободна.
        if (reply !== "adopt_new_session") decisionsInFlight.delete(tabId);
        return reply;
      },
      (error: unknown): HeldVideoReply => {
        dependencies.note("the viewer play decision failed, releasing the video", error);
        decisionsInFlight.delete(tabId);
        return "release";
      },
    );
  }

  return {
    heldVideo(tabId, frameId, report) {
      switch (report.trigger) {
        case "next_video":
          return dependencies.handoffs.heldVideo(tabId, frameId, report);
        case "viewer_play":
          return viewerPlay(tabId, frameId, report);
      }
    },
    viewerOfferChoice: (tabId, frameId, message) => viewerOffers.choose(tabId, frameId, message),
    tabRemoved: (tabId) => viewerOffers.forgetTab(tabId),
  };
}
