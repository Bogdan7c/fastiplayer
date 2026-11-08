// Вход «правый клик по ссылке» (сессия 10c, решение 217): ссылка открывается вкладкой СПЕРЕДИ (не в фоне),
// вход ждёт, пока на ней появится видео, отдаёт его плееру, а конец сеанса со стороны плеера закрывает вкладку.
//
// Почему не в фоне: в скрытой вкладке, куда ни разу не заходили, видео на MSE в Chromium не стартует вовсе.
//
// Жизнь вкладки входа:
//  1. `tabs.create` (активная, рядом с исходной) -> вкладка «наша», вход ждёт.
//  2. Опрос `surveyTab` раз в `LINK_POLL_INTERVAL_MS`, каждый следующий — после завершения предыдущего.
//     Видео нет / страница ещё грузится (`unavailable`) — ждём. Есть главное видео — `handOffTab`.
//     `no_video` от `handOffTab` (видео пропало между опросом и передачей) — тоже ждём.
//  3. Удача — вкладка остаётся нашей до конца сеанса. Любой другой исход или истёкший срок — плашка на этой
//     вкладке, вкладка НЕ закрывается и больше не наша: страницу перед глазами человека не закрываем.
//  4. Закрывается вкладка только концом сеанса со стороны плеера (`returned_to_page`, `player_ended`);
//     «Return» зрителя с заглушки (`returned_by_viewer`, решение 236) вкладку НЕ закрывает: видео играет на ней;
//     страница забрала видео, ушла и т. п. — вкладка уже человека, забываем её. Замена видео на той же
//     вкладке тоже зовёт `onSessionEnded`, но сеанс продолжается (вкладка есть среди `activeTabIds`).
//  5. Человек закрыл вкладку сам (`tabs.onRemoved`) — ожидание прекращается, без сообщений.
//
// Связь «вкладка -> вход» живёт ТОЛЬКО в памяти фона: после смерти фона (service worker уснул и очистился)
// сеанс всё равно мёртв, а вкладка просто остаётся открытой. Осознанное ограничение: хранить её в
// `storage.session` ради закрытия вкладки значило бы воскрешать вход, чего человек не просил.

import type { SurveyTabReply } from "../../shared/survey-contract.ts";
import { sameRef } from "../tab-model/tab-snapshot.ts";
import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { HandoffSessionEnd } from "../handoff/session-contract.ts";
import type { SessionCloseReason } from "../../protocol/session/close-reason.ts";
import type { EntryReporter } from "./entry-reporter.ts";

/**
 * Как часто опрашиваем вкладку входа. Секунда: страница грузится секунды, а опрос — это `executeScript` во все
 * фреймы; чаще нагружает страницу, реже заставляет человека ждать лишнее.
 */
export const LINK_POLL_INTERVAL_MS = 1_000;

/**
 * Сколько ждём видео на вкладке входа с момента её открытия. Минута: хватает на медленную загрузку и
 * ручной запуск видео; дольше — человек, скорее всего, ушёл, а вкладка остаётся открытой с плашкой.
 */
export const LINK_VIDEO_WAIT_MS = 60_000;

/** Откуда открыта ссылка: вкладка, окно и позиция исходной вкладки (новая встаёт сразу за ней). */
export interface LinkSource {
  readonly tabId: number;
  readonly windowId: number | undefined;
  readonly index: number | undefined;
}

/** Свойства новой вкладки; `active: true` — решение 217 — зашито во вход, а не в вызывающего. */
export interface LinkTabProperties {
  readonly url: string;
  readonly openerTabId: number;
  readonly windowId?: number;
  readonly index?: number;
}

/** Браузерные действия входа (в проде — `chrome.tabs`). Каждое отклоняется при сбое браузера. */
export interface LinkTabsApi {
  /** Открыть активную вкладку; возвращает её номер. */
  openActiveTab(properties: LinkTabProperties): Promise<number>;
  activateTab(tabId: number): Promise<void>;
  closeTab(tabId: number): Promise<void>;
  /** Вкладка сейчас выбрана в своём окне. Отклоняется или даёт `false`, если вкладки нет. */
  isTabActive(tabId: number): Promise<boolean>;
  /** Вкладка ещё существует (ответ `false`, а не отказ: «нет вкладки» — обычный ответ). */
  tabExists(tabId: number): Promise<boolean>;
}

/** Из команд передачи вход использует только эти. */
export interface LinkHandoffs {
  surveyTab(tabId: number): Promise<SurveyTabReply>;
  handOffTab(tabId: number): Promise<HandOffTabOutcome>;
  activeTabIds(): number[];
}

export interface LinkEntryDependencies {
  readonly tabs: LinkTabsApi;
  readonly handoffs: LinkHandoffs;
  readonly scheduler: Scheduler;
  readonly reporter: EntryReporter;
  readonly note: (message: string, error: unknown) => void;
}

export interface LinkEntry {
  /**
   * Открыть ссылку и вести её до конца. `linkUrl` — адрес из клика (`undefined` или не http(s) — вкладку не
   * открываем, плашка на исходной). Возвращает управление, когда ожидание видео кончилось. Не отклоняется.
   */
  openLink(linkUrl: string | undefined, source: LinkSource): Promise<void>;
  /** Сеанс вкладки кончился (`TabHandoffs.onSessionEnded`). */
  onSessionEnded(tabId: number, end: HandoffSessionEnd): void;
  /** `tabs.onRemoved`. */
  onTabRemoved(tabId: number): void;
  /** Вкладки, которые вход считает своими (для тестов и диагностики). */
  ownedTabIds(): number[];
}

/**
 * Причины закрытия сокета плеером, которые значат «окно плеера закрыли, доиграло или заменили» (нормальный
 * конец). Всё остальное (обрыв, неизвестная причина, нарушение протокола) — авария: видео вернулось на
 * страницу, и вкладку закрывать нельзя.
 */
const NORMAL_PLAYER_CLOSE_REASONS: readonly (SessionCloseReason | undefined)[] = ["player_closed", "replaced_by_another_tab", "player_shutting_down"];

/** Обычное закрытие WebSocket (код 1000): плеер закрыл соединение сам и чисто. */
const NORMAL_WEBSOCKET_CLOSE_CODE = 1000;

/**
 * Сеанс кончился нормально по воле плеера? Тогда вкладка входа закрывается.
 * «Return» с заглушки (`returned_by_viewer`, решения 51, 236) сюда не входит: видео играет дальше на этой
 * вкладке, а закрывает только конец, после которого смотреть больше нечего.
 */
function endsNormallyFromPlayerSide(end: HandoffSessionEnd): boolean {
  if (end.kind === "returned_to_page") return true;
  if (end.kind !== "player_ended" || end.cause.kind !== "player_closed") return false;
  return end.cause.code === NORMAL_WEBSOCKET_CLOSE_CODE || NORMAL_PLAYER_CLOSE_REASONS.includes(end.cause.reason);
}

/** Отказы передачи, которые проходят сами (страница грузится, опрос не успел): ждём дальше до срока. */
export function isTemporaryRefusal(outcome: HandOffTabOutcome): boolean {
  if (outcome.kind === "no_video" || outcome.kind === "video_gone") return true;
  // Страница только грузится, скрипт вот-вот встанет: на свежей вкладке это ещё не «вкладка старше скрипта».
  if (outcome.kind === "reload_needed") return true;
  return outcome.kind === "failed" && (outcome.reason === "survey_timed_out" || outcome.reason.startsWith("tab_not_scriptable"));
}

/** Запись о вкладке, открытой входом. */
interface OwnedTab {
  readonly sourceTabId: number;
  /** Вкладку закрыл человек: ожидание надо бросить молча. */
  removed: boolean;
  /** Будит спящий опрос (при закрытии вкладки не ждать секунду зря). */
  wake: (() => void) | undefined;
}

/** Адрес можно открыть вкладкой: только http и https (файлы и `javascript:` по ссылке не открываем). */
function isWebAddress(address: string): boolean {
  try {
    const { protocol } = new URL(address);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

export function createLinkEntry(dependencies: LinkEntryDependencies): LinkEntry {
  const { scheduler, handoffs, tabs, reporter } = dependencies;
  const owned = new Map<number, OwnedTab>();

  /** Пауза между опросами, которую можно прервать. */
  function pause(tab: OwnedTab, delayMs: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = scheduler.setTimeout(() => {
        tab.wake = undefined;
        resolve();
      }, delayMs);
      tab.wake = () => {
        scheduler.clearTimeout(timer);
        tab.wake = undefined;
        resolve();
      };
    });
  }

  /** Главное видео вкладки по этому опросу ИГРАЕТ (стоящее на паузе вход по ссылке не берёт, решение 217). */
  function mainVideoIsPlaying(survey: Extract<SurveyTabReply, { kind: "surveyed" }>): boolean {
    const main = survey.main;
    if (main === null) return false;
    const record = survey.videos.find((video) => sameRef({ frame_id: video.frame_id, document: video.document, element: video.element }, main));
    return record?.playing === true;
  }

  /**
   * Что делать после опроса. `wait` — ждём дальше; `settled` — вопрос решён (передача состоялась, отказ
   * получен или человек сам запустил видео); `gone` — вкладки уже нет.
   */
  async function stepOnSurvey(tabId: number): Promise<"wait" | "settled" | "gone"> {
    const survey = await handoffs.surveyTab(tabId);
    if (survey.kind !== "surveyed") {
      // Страница ещё грузится или перешла: ждём. Но вкладку могли закрыть до того, как мы её запомнили.
      return (await tabs.tabExists(tabId)) ? "wait" : "gone";
    }
    // Человек сам нажал значок: сеанс уже есть, вкладка остаётся за входом до его конца.
    if (survey.in_player.kind === "in_player") return "settled";
    // Видео на паузе ждёт настоящего play: попап берёт и такое (решение 203), а ссылка — нет.
    if (!mainVideoIsPlaying(survey)) return "wait";
    const outcome = await handoffs.handOffTab(tabId);
    if (isTemporaryRefusal(outcome)) return "wait";
    if (outcome.kind !== "started") release(tabId); // окончательный отказ: вкладка больше не наша
    await reporter.report(tabId, { kind: "outcome", outcome });
    return "settled";
  }

  async function waitForVideo(tabId: number, tab: OwnedTab): Promise<void> {
    const deadlineMs = scheduler.now() + LINK_VIDEO_WAIT_MS;
    while (!tab.removed && scheduler.now() < deadlineMs) {
      await pause(tab, LINK_POLL_INTERVAL_MS);
      if (tab.removed) return;
      let verdict: "wait" | "settled" | "gone";
      try {
        verdict = await stepOnSurvey(tabId);
      } catch (error) {
        dependencies.note("link entry poll failed, waiting on", error);
        verdict = "wait";
      }
      if (tab.removed) return;
      if (verdict === "gone") {
        // Вкладки нет: тихо конец, без плашки и без минуты пустых опросов.
        release(tabId);
        return;
      }
      if (verdict === "settled") return;
    }
    if (tab.removed) return;
    // Срок вышел: вкладка остаётся открытой, но больше не наша (страницу перед глазами не закрываем).
    release(tabId);
    await reporter.report(tabId, { kind: "link_no_video_started" });
  }

  function release(tabId: number): void {
    owned.delete(tabId);
  }

  async function openLink(linkUrl: string | undefined, source: LinkSource): Promise<void> {
    if (linkUrl === undefined || !isWebAddress(linkUrl)) {
      await reporter.report(source.tabId, { kind: "link_not_opened" });
      return;
    }
    const url = linkUrl;
    let tabId: number;
    try {
      tabId = await tabs.openActiveTab({
        url,
        openerTabId: source.tabId,
        ...(source.windowId === undefined ? {} : { windowId: source.windowId }),
        // Новая вкладка встаёт сразу за исходной.
        ...(source.index === undefined ? {} : { index: source.index + 1 }),
      });
    } catch (error) {
      dependencies.note("could not open the link tab", error);
      await reporter.report(source.tabId, { kind: "link_not_opened" });
      return;
    }
    const tab: OwnedTab = { sourceTabId: source.tabId, removed: false, wake: undefined };
    owned.set(tabId, tab);
    await waitForVideo(tabId, tab);
  }

  function onSessionEnded(tabId: number, end: HandoffSessionEnd): void {
    const tab = owned.get(tabId);
    if (tab === undefined) return; // не наша вкладка: обычная вкладка с сеансом не закрывается никогда
    // Замена видео на этой же вкладке: старый сеанс кончился, новый уже идёт. Ничего не делаем.
    if (handoffs.activeTabIds().includes(tabId)) return;
    release(tabId);
    // Страница забрала видео, ушла или плеер оборвался аварийно: вкладка уже человека.
    if (!endsNormallyFromPlayerSide(end)) return;
    void closeOwnedTab(tabId, tab);
  }

  /** Вернуть человека на исходную вкладку (если он на закрываемой) и закрыть вкладку входа. Запись уже снята: повтор невозможен. */
  async function closeOwnedTab(tabId: number, tab: OwnedTab): Promise<void> {
    // Возвращаем на исходную только того, кто смотрит на закрываемую вкладку: человек на третьей не прыгает.
    let wasActive = false;
    try {
      wasActive = await tabs.isTabActive(tabId);
    } catch {
      // Вкладки нет или браузер не ответил: возвращать некого.
    }
    if (wasActive) {
      try {
        // Исходной вкладки может уже не быть: тогда вызов отклонится, и это не мешает закрытию.
        await tabs.activateTab(tab.sourceTabId);
      } catch {
        // Исходная вкладка закрыта: браузер выберет соседнюю сам.
      }
    }
    try {
      await tabs.closeTab(tabId);
    } catch (error) {
      dependencies.note("could not close the link tab", error);
    }
  }

  function onTabRemoved(tabId: number): void {
    const tab = owned.get(tabId);
    if (tab === undefined) return;
    owned.delete(tabId);
    tab.removed = true;
    tab.wake?.();
  }

  return { openLink, onSessionEnded, onTabRemoved, ownedTabIds: () => [...owned.keys()] };
}
