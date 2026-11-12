// Ожидание видео после перезагрузки страницы в той же вкладке (сессия 16c, решения 59, 304, 305, 215).
//
// Документ страницы ушёл (полная перезагрузка или переход на другую страницу в ТОЙ ЖЕ вкладке), а сеанс не
// кончается сразу. Модуль владеет тем, что отличает это ожидание от прочих:
// - вопросом браузеру «где теперь вкладка»: закрыта, другой сайт или тот же (основной домен адреса вкладки,
//   решение 304, `shared/base-site.ts`);
// - сроком ожидания: посреди серии — настройка (решение 305, по умолчанию 10 с), после конца серии — срок 16a
//   (`NEXT_EPISODE_WAIT_MS`, решения 303, 308). Срок один: переезд (`video-relocation.ts`) на это время только
//   держит сторож взведённым и своего срока не начинает;
// - адресом вкладки за время ожидания (`tabs.onUpdated`): ушли на другой сайт — конец сразу, ждать нечего;
// - решением о задержанном видео нового документа: то же правило, что у переезда (`judgeHeldVideo`), плюс тот
//   же сайт. Подходит — сеанс отдаёт видео сеансу-замене (`PageReloadHandover`); пока владелец сеансов открывает
//   замену, срок не срабатывает (замену не обрывают на полпути). Не состоялась — ожидание продолжается остаток
//   ПРЕЖНЕГО срока; если он за время замены уже вышел — конец сразу.
//
// Фазой, сокетом и портом владеет сеанс (`handoff-session.ts`): сюда приходят только узкие вопросы, а концы
// уходят обратно через `siteLeft` и `waitExpired`. Таймеры — через `Scheduler` фона.

import type { HeldVideoReport } from "../../shared/handoff-contract.ts";
import { isSameBaseSite } from "../../shared/base-site.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../shared/next-episode-wait.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import type { FileAddressRule } from "./file-address-guard.ts";
import type { TabDescription } from "./handoff-session-dependencies.ts";
import { consentOf, judgeHeldVideo } from "./held-video-choice.ts";
import type { PlayerAbilities } from "./hls-player-support.ts";
import type { SessionHeldVideoAnswer } from "./session-contract.ts";
import type { WatchTabUpdates } from "./episode-change/session-episode-change.ts";

/**
 * Каким было видео, когда документ ушёл:
 * - `mid_episode` — серия ещё шла: окно досматривает скачанное, срок — настройка; по сроку — уход страницы;
 * - `after_episode_end` — серия уже доиграла у плеера (как 16a): срок `NEXT_EPISODE_WAIT_MS`, по сроку — конец 16a.
 */
export type PageReloadWaitKind = "mid_episode" | "after_episode_end";

/** Где теперь вкладка, ушедший документ которой был страницей сеанса. */
export type ReloadedTabAnswer = "same_site" | "other_site" | "closed";

/** Что модулю нужно от браузера (в сеансе — его зависимости как есть). */
export interface SessionPageReloadBrowser {
  readonly scheduler: Scheduler;
  readonly watchTabUpdates: WatchTabUpdates;
  /** `tabs.get`; `undefined` — вкладки нет. */
  readonly describeTab: (tabId: number) => Promise<TabDescription | undefined>;
  readonly noteForDeveloper: (message: string) => void;
}

export interface SessionPageReloadPorts {
  readonly browser: SessionPageReloadBrowser;
  /** Вкладка страницы сеанса сейчас. */
  readonly currentTabId: () => number;
  /** Адрес страницы сеанса при старте: с его сайтом сравнивается адрес вкладки. */
  readonly sessionPageUrl: () => string | undefined;
  /** Срок посреди серии из настройки (решение 305); `0` — не ждать вовсе. */
  readonly midEpisodeWaitMs: number;
  /** Правило адреса файла сеанса и возможности плеера: по ним судится задержанное видео. */
  readonly fileAddressRule: () => FileAddressRule;
  readonly playerAbilities: () => PlayerAbilities;
  /** Во время ожидания вкладка ушла на другой сайт: прежний конец «ушли со страницы». */
  readonly siteLeft: () => void;
  /** Срок вышел, видео не пришло (или замена не состоялась после срока). */
  readonly waitExpired: (kind: PageReloadWaitKind) => void;
}

export interface SessionPageReload {
  /** Ждать ли вообще (настройка не `0`). */
  waitsForReloadedPage(): boolean;
  /**
   * Порт документа умер: где теперь вкладка. Подписка на адрес вкладки ставится ДО вопроса, чтобы смена адреса
   * в промежутке не потерялась. Вопрос один за раз: повторный, пока первый в пути, ничего не делает.
   */
  askWhereTabWent(onAnswer: (answer: ReloadedTabAnswer) => void): void;
  /** Начать ожидание: вкладка на том же сайте, сеанс перешёл в фазу `page_reloading`. */
  begin(kind: PageReloadWaitKind): void;
  /** Задержанное видео во время ожидания: отпустить или отдать сеансу-замене. */
  heldVideo(frameId: number, report: HeldVideoReport): SessionHeldVideoAnswer;
  /** Замена не состоялась: ждать остаток прежнего срока; он уже вышел — конец сразу. */
  handoverRefused(): void;
  /** Ожидание больше не нужно (сеанс кончился, стал сиротой или замена состоялась). Повторный вызов безвреден. */
  stop(): void;
}

/** Состояние ожидания. */
type ReloadState =
  | { readonly kind: "idle" }
  /** Вопрос «где вкладка» в пути. */
  | { readonly kind: "asking" }
  /** Ждём видео нового документа до `deadlineAtMs`. */
  | { readonly kind: "waiting"; readonly wait: PageReloadWaitKind; readonly deadlineAtMs: number }
  /** Видео отдано сеансу-замене; срок `deadlineAtMs` не срабатывает, пока замена открывается. */
  | { readonly kind: "handing_over"; readonly wait: PageReloadWaitKind; readonly deadlineAtMs: number };

const IDLE: ReloadState = { kind: "idle" };

export function createSessionPageReload(ports: SessionPageReloadPorts): SessionPageReload {
  const deadline = createTimerSlot(ports.browser.scheduler);
  let state: ReloadState = IDLE;
  // Последний известный адрес вкладки: из ответа браузера или из новости `tabs.onUpdated`, что новее.
  let latestTabUrl: string | undefined;
  // Новость об адресе пришла после вопроса: ответ на вопрос старее её и адрес не перезаписывает.
  let urlUpdatedSinceQuestion = false;
  let unsubscribe: (() => void) | undefined;

  function limitMsOf(wait: PageReloadWaitKind): number {
    return wait === "after_episode_end" ? NEXT_EPISODE_WAIT_MS : ports.midEpisodeWaitMs;
  }

  function onTheSessionSite(url: string | undefined): boolean {
    return isSameBaseSite(ports.sessionPageUrl(), url);
  }

  function watchTab(): void {
    if (unsubscribe !== undefined) return;
    unsubscribe = ports.browser.watchTabUpdates((update) => {
      if (update.tabId !== ports.currentTabId() || update.url === undefined) return;
      latestTabUrl = update.url;
      urlUpdatedSinceQuestion = true;
      // Ушли на другой сайт посреди ожидания: видео той страницы сеанс не возьмёт, ждать нечего.
      if ((state.kind === "waiting" || state.kind === "handing_over") && !onTheSessionSite(update.url)) ports.siteLeft();
    });
  }

  function stop(): void {
    deadline.cancel();
    state = IDLE;
    unsubscribe?.();
    unsubscribe = undefined;
  }

  /** Срок вышел: ожидание кончено, конец выбирает сеанс по виду ожидания. */
  function expire(wait: PageReloadWaitKind): void {
    stop();
    ports.waitExpired(wait);
  }

  function waitUntil(wait: PageReloadWaitKind, deadlineAtMs: number): void {
    state = { kind: "waiting", wait, deadlineAtMs };
    deadline.start(deadlineAtMs - ports.browser.scheduler.now(), () => expire(wait));
  }

  function answerOf(tab: TabDescription | undefined): ReloadedTabAnswer {
    if (tab === undefined) return "closed";
    if (!urlUpdatedSinceQuestion) latestTabUrl = tab.url;
    return onTheSessionSite(latestTabUrl) ? "same_site" : "other_site";
  }

  return {
    waitsForReloadedPage: () => ports.midEpisodeWaitMs > 0,
    askWhereTabWent(onAnswer) {
      if (state.kind !== "idle") return;
      state = { kind: "asking" };
      urlUpdatedSinceQuestion = false;
      watchTab();
      const settle = (answer: ReloadedTabAnswer): void => {
        // Сеанс успел кончиться или стать сиротой (`stop`): ответ старому документу больше не нужен.
        if (state.kind !== "asking") return;
        state = IDLE;
        onAnswer(answer);
      };
      ports.browser.describeTab(ports.currentTabId()).then(
        (tab) => settle(answerOf(tab)),
        (error: unknown) => {
          // Браузер не ответил о вкладке — как и в session-page-presence.ts, считаем её закрытой, но не молча.
          ports.browser.noteForDeveloper(`page reload: tabs.get failed, the tab is treated as closed: ${String(error)}`);
          settle("closed");
        },
      );
    },
    begin(wait) {
      watchTab();
      waitUntil(wait, ports.browser.scheduler.now() + limitMsOf(wait));
    },
    heldVideo(frameId, report) {
      // Пока замена открывается, другого видео сеанс не берёт: одна замена за раз.
      if (state.kind !== "waiting") return "release";
      if (!onTheSessionSite(latestTabUrl)) return "release";
      const decision = judgeHeldVideo(frameId, report, ports.fileAddressRule(), ports.playerAbilities());
      if (decision.kind === "release") return "release";
      deadline.cancel();
      state = { kind: "handing_over", wait: state.wait, deadlineAtMs: state.deadlineAtMs };
      const { frameId: videoFrameId, document, element, role, mayBeAd } = decision.video;
      return {
        kind: "replace_session",
        video: { frame_id: videoFrameId, document, element },
        consent: consentOf(decision.fileAddressRule),
        judged: { role, mayBeAd },
      };
    },
    handoverRefused() {
      if (state.kind !== "handing_over") return;
      const { wait, deadlineAtMs } = state;
      // Срок вышел, пока открывалась замена: ждать больше нечего.
      if (deadlineAtMs <= ports.browser.scheduler.now()) return expire(wait);
      waitUntil(wait, deadlineAtMs);
    },
    stop,
  };
}
