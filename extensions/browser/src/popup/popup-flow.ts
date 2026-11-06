// Шаги попапа без DOM (решение 209): активная вкладка -> опрос -> (передача главного видео | список) ->
// выбор строки -> согласие на локальный адрес. Все зависимости приходят снаружи, поэтому весь сценарий
// проверяется в node:test без браузера. Закрытие попапа ничего не отменяет: передачу держит фон.

import type { HandOffTabOutcome, VideoRef } from "../shared/handoff-contract.ts";
import type { SurveyTabReply } from "../shared/survey-contract.ts";
import {
  DECLINED_NOTICE,
  describeConsentQuestion,
  describeOutcome,
  describeUnavailable,
  LOADING_NOTICE,
  NO_ACTIVE_TAB_NOTICE,
  PLAYING_NOTICE,
  type ConsentQuestionView,
  type NoticeView,
} from "./outcome-text.ts";
import { describeVideoList, type VideoRowView } from "./video-row-text.ts";

/** Активная вкладка окна, из которого открыт попап. */
export interface ActiveTab {
  readonly id: number;
  /** Заголовок; пусто, если браузер его не отдаёт (страницы браузера). */
  readonly title: string;
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
}

export function createPopupFlow(dependencies: PopupFlowDependencies): PopupFlow {
  let tab: ActiveTab | undefined;
  let surveyed: Surveyed | undefined;
  let notice: NoticeView = LOADING_NOTICE;
  let pendingConsent: PendingConsent | undefined;
  let busy = false;

  function publish(): void {
    const rows = surveyed === undefined ? [] : describeVideoList(surveyed.videos, tab?.title ?? "", inPlayerVideo(surveyed));
    dependencies.render({ notice, question: pendingConsent === undefined ? null : describeConsentQuestion(pendingConsent.host), rows, busy });
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
    publish();
    let outcome: HandOffTabOutcome;
    try {
      outcome = await dependencies.handOffTab(request);
    } catch {
      outcome = { kind: "failed", reason: "no_answer_from_background" };
    }
    if (outcome.kind === "started") {
      // Передачу держит фон; попап не нужен. Модель не перерисовываем: окно закрывается.
      dependencies.closePopup();
      return;
    }
    busy = false;
    notice = describeOutcome(outcome);
    if (outcome.kind === "local_address_needs_consent") pendingConsent = { host: outcome.host, video: request.video };
    // Выбранного видео уже нет: список устарел, берём свежий (опрос заодно сбросит старый отказ).
    if (outcome.kind === "video_gone" && tab !== undefined) acceptSurvey(await survey(tab.id));
    publish();
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

  return { start, chooseVideo, allowLocalAddress, declineLocalAddress };
}
