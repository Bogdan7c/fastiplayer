// Поздний вход (сессия 10d, решение 210): вкладка открыта раньше, чем встал наш скрипт, и человек нажал
// «Reload and open». Владеет ходом этой работы. Передача потом обычная: `handOffTab`, нового пути нет.
//
// Ход работы:
//  1. Запомнить секунду видео и метки документов, которые есть сейчас.
//  2. `tabs.reload`. Без нажатия кнопки этого не случается никогда (человек мог что-то вводить).
//  3. Раз в секунду опрашивать вкладку. Видео из старых документов не считаются: перезагрузка могла ещё не
//     начаться или застрять на вопросе сайта «покинуть страницу?», а старое видео за новое принимать нельзя.
//  4. Шаг решает `decideLateStep`: ждать, поставить секунду или передать.
//  5. Видео долго не играет — плашка «Press play on the page». Передача случится после нажатия.
//
// Вкладку закрыли или человек ушёл на другой сайт — работа молча снимается: это его решение, а не сбой.
// Срок вышел — плашка: страница остаётся как есть.
//
// Всё живёт ТОЛЬКО в памяти фона. Перезагрузка вкладки фон не трогает, а если браузер его усыпит, поздний
// вход просто не случится, и человек нажмёт значок сам: воскрешать работу из хранилища незачем.

import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import type { SurveyTabReply } from "../../shared/survey-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { sameRef, type TabVideoRecord } from "../tab-model/tab-snapshot.ts";
import type { EntryReporter } from "./entry-reporter.ts";
import { isTemporaryRefusal } from "./link-entry.ts";
import { decideLateStep, isForeignVideo, NO_SEEK_YET, rememberedVideoOf, type LatePositionResult, type LateSeekProgress, type LateStep, type RememberedVideo } from "./late-entry-step.ts";
import type { LateVideoFact, LateVideoReader } from "./late-video-reader.ts";

/** Как часто смотрим на вкладку: как у входа по ссылке, чаще незачем нагружать страницу. */
export const LATE_POLL_INTERVAL_MS = 1_000;

/**
 * Сколько ждём, что видео заиграет. Две минуты: человеку надо заметить подсказку и найти кнопку play,
 * а реклама перед фильмом тоже идёт в этот срок.
 */
export const LATE_PLAY_WAIT_MS = 120_000;

/** Через сколько после перезагрузки подсказать «нажми play»: автозапуск к этому времени уже случился бы. */
export const LATE_PRESS_PLAY_HINT_MS = 6_000;

/** Браузерные действия. Каждое отклоняется при сбое браузера. */
export interface LateTabsApi {
  reload(tabId: number): Promise<void>;
  /** Откуда страница вкладки сейчас; `undefined` — вкладки нет или адрес не читается. */
  originOf(tabId: number): Promise<string | undefined>;
}

/** Из команд передачи вход использует только эти. */
export interface LateHandoffs {
  surveyTab(tabId: number): Promise<SurveyTabReply>;
  handOffTab(tabId: number): Promise<HandOffTabOutcome>;
}

export interface LateEntryDependencies {
  readonly tabs: LateTabsApi;
  readonly videos: LateVideoReader;
  readonly handoffs: LateHandoffs;
  readonly scheduler: Scheduler;
  readonly reporter: EntryReporter;
  readonly note: (message: string, error: unknown) => void;
}

export interface LateEntry {
  /**
   * Человек нажал кнопку. Управление возвращается сразу, работа идёт в фоне. `false` — на этой вкладке
   * работа уже идёт: второе нажатие вторую перезагрузку не вызывает.
   */
  start(tabId: number): boolean;
  isRunning(tabId: number): boolean;
}

/** Состояние одной работы. */
interface LateJob {
  readonly tabId: number;
  readonly origin: string;
  readonly remembered: RememberedVideo | null;
  /** Документы до перезагрузки: их видео уже не считаются. */
  readonly staleDocuments: ReadonlySet<string>;
  readonly reloadedAtMs: number;
  seek: LateSeekProgress;
  foreignVideoPlayingSinceMs: number | null;
  hintShown: boolean;
}

/** Что видно на вкладке после перезагрузки. */
interface FreshView {
  readonly main: TabVideoRecord | undefined;
  readonly anyPlaying: boolean;
}

function freshViewOf(survey: Extract<SurveyTabReply, { kind: "surveyed" }>, staleDocuments: ReadonlySet<string>): FreshView {
  const fresh = survey.videos.filter((video) => !staleDocuments.has(video.document));
  const { main } = survey;
  const mainRecord = main === null ? undefined : fresh.find((video) => sameRef({ frame_id: video.frame_id, document: video.document, element: video.element }, main));
  return { main: mainRecord, anyPlaying: fresh.some((video) => video.playing) };
}

export function createLateEntry(dependencies: LateEntryDependencies): LateEntry {
  const { tabs, videos, handoffs, scheduler, reporter, note } = dependencies;
  const running = new Set<number>();

  function pause(delayMs: number): Promise<void> {
    return new Promise((resolve) => void scheduler.setTimeout(resolve, delayMs));
  }

  async function readVideosOrNothing(tabId: number): Promise<LateVideoFact[]> {
    try {
      return await videos.read(tabId);
    } catch (error) {
      note("late entry could not read the videos of the tab", error);
      return [];
    }
  }

  /** Метки документов сейчас. Сбой опроса не срывает работу: в Chrome их и так нет, скрипта-то нет. */
  async function currentDocuments(tabId: number): Promise<ReadonlySet<string>> {
    const survey = await handoffs.surveyTab(tabId);
    return new Set(survey.kind === "surveyed" ? survey.videos.map((video) => video.document) : []);
  }

  /** Передать и сказать итог. `true` — работа кончена, `false` — отказ временный, ждём дальше. */
  async function handOff(job: LateJob, position: LatePositionResult): Promise<boolean> {
    const outcome = await handoffs.handOffTab(job.tabId);
    if (isTemporaryRefusal(outcome)) return false;
    const fromSitePosition = position === "site_position" && outcome.kind === "started" && outcome.player_window === "new";
    await reporter.report(job.tabId, fromSitePosition ? { kind: "late_started_from_site_position" } : { kind: "outcome", outcome });
    return true;
  }

  /** Запомнить, с какого момента играет чужое видео (другая длительность): от него отсчитывается ожидание фильма. */
  function trackForeignVideo(job: LateJob, view: FreshView, nowMs: number): void {
    const { remembered } = job;
    if (remembered === null || view.main === undefined || !view.main.playing) return;
    // Фильм показался — отсчёт сброшен: реклама, вставшая после него, ждёт свой полный срок.
    job.foreignVideoPlayingSinceMs = isForeignVideo(view.main.duration_s, remembered) ? (job.foreignVideoPlayingSinceMs ?? nowMs) : null;
  }

  /** Подсказать «нажми play» один раз, если ничего не играет. */
  async function hintIfNothingPlays(job: LateJob, view: FreshView, nowMs: number): Promise<void> {
    if (job.hintShown || view.anyPlaying || nowMs - job.reloadedAtMs < LATE_PRESS_PLAY_HINT_MS) return;
    job.hintShown = true;
    await reporter.report(job.tabId, { kind: "late_press_play" });
  }

  /** Исполнить решение. `true` — работа кончена. */
  async function perform(job: LateJob, step: LateStep, nowMs: number): Promise<boolean> {
    switch (step.kind) {
      case "wait":
        return false;
      case "seek":
        await videos.seek(job.tabId, step.target);
        job.seek = { attempts: job.seek.attempts + 1, lastAttemptAtMs: nowMs };
        return false;
      case "hand_off":
        return handOff(job, step.position);
    }
  }

  /** Один шаг после паузы. `true` — работа кончена. */
  async function step(job: LateJob): Promise<boolean> {
    const survey = await handoffs.surveyTab(job.tabId);
    if (survey.kind !== "surveyed") return false; // страница ещё грузится
    if (survey.in_player.kind === "in_player") return true; // человек сам отдал видео другим входом
    const view = freshViewOf(survey, job.staleDocuments);
    const nowMs = scheduler.now();
    trackForeignVideo(job, view, nowMs);
    await hintIfNothingPlays(job, view, nowMs);
    const decision = decideLateStep({
      remembered: job.remembered,
      mainDurationS: view.main === undefined ? undefined : view.main.duration_s,
      mainPlaying: view.main?.playing === true,
      videos: await readVideosOrNothing(job.tabId),
      seek: job.seek,
      nowMs,
      foreignVideoPlayingSinceMs: job.foreignVideoPlayingSinceMs,
    });
    return perform(job, decision, nowMs);
  }

  async function follow(job: LateJob): Promise<void> {
    const deadlineMs = job.reloadedAtMs + LATE_PLAY_WAIT_MS;
    while (scheduler.now() < deadlineMs) {
      await pause(LATE_POLL_INTERVAL_MS);
      // Вкладки нет или это уже другой сайт: человек решил сам, молча уходим.
      if ((await tabs.originOf(job.tabId)) !== job.origin) return;
      try {
        if (await step(job)) return;
      } catch (error) {
        note("late entry step failed, waiting on", error);
      }
    }
    await reporter.report(job.tabId, { kind: "late_no_video_started" });
  }

  async function run(tabId: number): Promise<void> {
    const origin = await tabs.originOf(tabId);
    if (origin === undefined) {
      // Человек нажал кнопку и попап уже закрылся: молчать нельзя, иначе нажатие выглядит пустым.
      note("late entry could not read the tab address", new Error("the tab is gone or its address is hidden"));
      await reporter.report(tabId, { kind: "late_reload_failed" });
      return;
    }
    const remembered = rememberedVideoOf(await readVideosOrNothing(tabId));
    const staleDocuments = await currentDocuments(tabId);
    try {
      await tabs.reload(tabId);
    } catch (error) {
      note("late entry could not reload the tab", error);
      await reporter.report(tabId, { kind: "late_reload_failed" });
      return;
    }
    await follow({ tabId, origin, remembered, staleDocuments, reloadedAtMs: scheduler.now(), seek: NO_SEEK_YET, foreignVideoPlayingSinceMs: null, hintShown: false });
  }

  return {
    start(tabId) {
      if (running.has(tabId)) return false;
      running.add(tabId);
      run(tabId)
        .catch((error: unknown) => note("late entry failed", error))
        .finally(() => running.delete(tabId));
      return true;
    },
    isRunning: (tabId) => running.has(tabId),
  };
}
