// Шаги попапа без DOM (решение 209): активная вкладка -> опрос -> (передача главного видео | список) ->
// выбор строки -> согласие на локальный адрес или «Reload and open» (10d). Все зависимости приходят снаружи, поэтому весь сценарий
// проверяется в node:test без браузера. Закрытие попапа ничего не отменяет: передачу держит фон.

import type { HandOffTabOutcome, VideoRef } from "../shared/handoff-contract.ts";
import type { SurveyTabReply } from "../shared/survey-contract.ts";
import {
  DECLINED_NOTICE,
  describeConsentQuestion,
  describeOutcome,
  describeUnavailable,
  LATE_ENTRY_RUNNING_NOTICE,
  LOADING_NOTICE,
  NO_ACTIVE_TAB_NOTICE,
  PLAYING_NOTICE,
  RELOAD_AND_OPEN_LABEL,
  type ConsentQuestionView,
  type NoticeView,
} from "./outcome-text.ts";
import { describeVideoList, type VideoRowView } from "./video-row-text.ts";

/** Активная вкладка окна, из которого открыт попап. */
export interface ActiveTab {
  readonly id: number;
  /** Заголовок; пусто, если браузер его не отдаёт (страницы браузера). */
  readonly title: string;
  /** Адрес вкладки; нет, если браузер его не отдаёт (нет прав на сайт, страницы браузера). */
  readonly url?: string;
}

/** Что попап просит у фона в команде «отдать вкладку». */
export interface HandOffRequest {
  readonly tabId: number;
  /** Выбранная строка списка; без неё фон отдаёт главное видео. */
  readonly video?: VideoRef;
  /** Хост, на который человек согласился (решение 199). */
  readonly consentedLocalHost?: string;
}

export interface PopupFlowDependencies {
  /** `tabs.query({active, currentWindow})`; `undefined` — вкладки нет. Отклонение считается «нет вкладки». */
  findActiveTab(): Promise<ActiveTab | undefined>;
  /** Команда `survey_tab`. Отклонение (фон не ответил) считается сбоем опроса. */
  surveyTab(tabId: number): Promise<SurveyTabReply>;
  /** Команда `hand_off_tab`. Отклонение считается исходом `failed`. */
  handOffTab(request: HandOffRequest): Promise<HandOffTabOutcome>;
  /** Фон сейчас ведёт «Reload and open» этой вкладки. Отклонение считается «нет». */
  isLateEntryRunning(tabId: number): Promise<boolean>;
  /** Команда `reload_and_open`: фон перезагружает вкладку и дальше работает сам. Отклоняется, если фон не ответил. */
  reloadAndOpen(tabId: number): Promise<void>;
  /** `window.close()`: передача началась, попап своё дело сделал. */
  closePopup(): void;
  /** Показать модель. Вызывается после каждого шага. */
  render(model: PopupModel): void;
}

/** Всё, что попапу нужно нарисовать. Строки уже готовые, DOM только раскладывает их. */
export interface PopupModel {
  readonly notice: NoticeView;
  /** Вопрос про локальный адрес; кнопки Allow / Don't allow. */
  readonly question: ConsentQuestionView | null;
  /** Подпись кнопки «Reload and open»; `null` — кнопки нет. */
  readonly reloadLabel: string | null;
  /** Найденные видео (пусто, пока опроса нет или страница закрыта для экстеншена). */
  readonly rows: readonly VideoRowView[];
  /** Идёт команда фону: нажатия игнорируются. */
  readonly busy: boolean;
}

/** Согласие, которого ждём от человека: хост и видео, чью команду повторим. */
interface PendingConsent {
  readonly host: string;
  readonly video: VideoRef | undefined;
}

type Surveyed = Extract<SurveyTabReply, { kind: "surveyed" }>;

export interface PopupFlow {
  /** Открытие попапа: вкладка -> опрос -> передача главного видео или список. */
  start(): Promise<void>;
  /** Клик по строке списка: передать именно это видео. */
  chooseVideo(video: VideoRef): Promise<void>;
  /** «Allow»: повторить команду с согласием на хост. */
  allowLocalAddress(): Promise<void>;
  /** «Don't allow»: видео остаётся в браузере. */
  declineLocalAddress(): void;
  /** «Reload and open»: единственный путь к перезагрузке вкладки (решение 210). */
  reloadAndOpen(): Promise<void>;
}

export function createPopupFlow(dependencies: PopupFlowDependencies): PopupFlow {
  let tab: ActiveTab | undefined;
  let surveyed: Surveyed | undefined;
  let notice: NoticeView = LOADING_NOTICE;
  let pendingConsent: PendingConsent | undefined;
  let reloadOffered = false;
  let busy = false;

  function publish(): void {
    const rows = surveyed === undefined ? [] : describeVideoList(surveyed.videos, tab?.title ?? "", inPlayerVideo(surveyed));
    dependencies.render({
      notice,
      question: pendingConsent === undefined ? null : describeConsentQuestion(pendingConsent.host),
      reloadLabel: reloadOffered ? RELOAD_AND_OPEN_LABEL : null,
      rows,
      busy,
    });
  }

  function inPlayerVideo(survey: Surveyed): VideoRef | null {
    return survey.in_player.kind === "in_player" ? survey.in_player.video : null;
  }

  /** Опрос; сбой связи с фоном — `survey_failed`, а не необработанное отклонение. */
  async function survey(tabId: number): Promise<SurveyTabReply> {
    try {
      return await dependencies.surveyTab(tabId);
    } catch {
      return { kind: "unavailable", reason: "survey_failed" };
    }
  }

  /** Принять ответ опроса. `true` — список построен, `false` — вкладку опросить нельзя (текст уже готов). */
  function acceptSurvey(reply: SurveyTabReply): boolean {
    if (reply.kind === "unavailable") {
      surveyed = undefined;
      notice = describeUnavailable(reply.reason);
      return false;
    }
    surveyed = reply;
    return true;
  }

  /** Команда фону. `started` закрывает попап, любой отказ оставляет его с текстом причины. */
  async function handOff(request: HandOffRequest): Promise<void> {
    busy = true;
    pendingConsent = undefined;
    reloadOffered = false;
    publish();
    let outcome: HandOffTabOutcome;
    try {
      outcome = await dependencies.handOffTab(request);
    } catch {
      outcome = { kind: "failed", reason: "no_answer_from_background" };
    }
    if (outcome.kind === "started" && outcome.player_window === "new") {
      // Новое окно плеера забрало фокус; передачу держит фон, попап не нужен. Модель не перерисовываем: окно закрывается.
      dependencies.closePopup();
      return;
    }
    busy = false;
    notice = describeOutcome(outcome);
    if (outcome.kind === "local_address_needs_consent") pendingConsent = { host: outcome.host, video: request.video };
    reloadOffered = outcome.kind === "reload_needed";
    // Выбранного видео уже нет: список устарел, берём свежий (опрос заодно сбросит старый отказ).
    if (outcome.kind === "video_gone" && tab !== undefined) acceptSurvey(await survey(tab.id));
    // Окно плеера уже было открыто и видео в нём сменилось: в списке теперь другая строка «in player».
    // Совет «переключись на окно» не должен пропасть, если опрос не удался: заменяем только сам список.
    if (outcome.kind === "started" && tab !== undefined) {
      const fresh = await survey(tab.id);
      if (fresh.kind === "surveyed") surveyed = fresh;
    }
    publish();
  }

  async function lateEntryRunning(tabId: number): Promise<boolean> {
    try {
      return await dependencies.isLateEntryRunning(tabId);
    } catch {
      return false;
    }
  }

  async function start(): Promise<void> {
    publish();
    try {
      tab = await dependencies.findActiveTab();
    } catch {
      tab = undefined;
    }
    if (tab === undefined) {
      notice = NO_ACTIVE_TAB_NOTICE;
      publish();
      return;
    }
    // Фон уже ведёт эту вкладку после «Reload and open». Передача отсюда опередила бы постановку на секунду.
    if (await lateEntryRunning(tab.id)) {
      notice = LATE_ENTRY_RUNNING_NOTICE;
      publish();
      return;
    }
    const reply = await survey(tab.id);
    if (!acceptSurvey(reply)) {
      publish();
      return;
    }
    if (reply.kind === "surveyed" && reply.in_player.kind === "in_player") {
      // Видео этой вкладки уже в плеере: ничего не отдаём (решение 209, п. 2), список остаётся для замены.
      notice = PLAYING_NOTICE;
      publish();
      return;
    }
    await handOff({ tabId: tab.id });
  }

  async function chooseVideo(video: VideoRef): Promise<void> {
    if (busy || tab === undefined) return;
    await handOff({ tabId: tab.id, video });
  }

  async function allowLocalAddress(): Promise<void> {
    if (busy || tab === undefined || pendingConsent === undefined) return;
    const { host, video } = pendingConsent;
    // Та же команда с тем же выбором и согласием на этот хост.
    await handOff(video === undefined ? { tabId: tab.id, consentedLocalHost: host } : { tabId: tab.id, video, consentedLocalHost: host });
  }

  function declineLocalAddress(): void {
    if (busy || pendingConsent === undefined) return;
    pendingConsent = undefined;
    notice = DECLINED_NOTICE;
    publish();
  }

  async function reloadAndOpen(): Promise<void> {
    if (busy || tab === undefined || !reloadOffered) return;
    busy = true;
    publish();
    try {
      await dependencies.reloadAndOpen(tab.id);
    } catch {
      busy = false;
      notice = describeOutcome({ kind: "failed", reason: "no_answer_from_background" });
      publish();
      return;
    }
    // Дальше всё ведёт фон, итог скажет плашка на странице: клик по play закрыл бы попап в любом случае.
    dependencies.closePopup();
  }

  return { start, chooseVideo, allowLocalAddress, declineLocalAddress, reloadAndOpen };
}
