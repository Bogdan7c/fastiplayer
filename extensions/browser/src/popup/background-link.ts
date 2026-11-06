// Настоящие зависимости попапа: активная вкладка и две команды фону. Ответ фона проверяем по форме:
// отсутствующий или кривой ответ (фон не слушает, отвечает чужой слушатель) — отклонение, а не «тихая» ошибка.

import { HAND_OFF_TAB_KIND, type HandOffTabOutcome } from "../shared/handoff-contract.ts";
import { SURVEY_TAB_KIND, type SurveyTabReply } from "../shared/survey-contract.ts";
import type { ActiveTab, HandOffRequest } from "./popup-flow.ts";

const OUTCOME_KINDS: readonly HandOffTabOutcome["kind"][] = [
  "started",
  "no_video",
  "not_supported_yet",
  "protected",
  "video_gone",
  "local_address_refused",
  "local_address_needs_consent",
  "player_unavailable",
  "failed",
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Ответ на `hand_off_tab` — известный вид исхода. Поля вида фон гарантирует сам (он наш). */
export function isHandOffTabOutcome(value: unknown): value is HandOffTabOutcome {
  return isRecord(value) && OUTCOME_KINDS.some((kind) => kind === value["kind"]);
}

/** Ответ на `survey_tab` — известный вид ответа. */
export function isSurveyTabReply(value: unknown): value is SurveyTabReply {
  return isRecord(value) && (value["kind"] === "surveyed" || value["kind"] === "unavailable");
}

export interface BackgroundLink {
  findActiveTab(): Promise<ActiveTab | undefined>;
  surveyTab(tabId: number): Promise<SurveyTabReply>;
  handOffTab(request: HandOffRequest): Promise<HandOffTabOutcome>;
}

export function createBackgroundLink(): BackgroundLink {
  return {
    async findActiveTab() {
      // Права на сайты (host_permissions) дают заголовок вкладки без права `tabs`; у страниц браузера он пуст.
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      return tab?.id === undefined ? undefined : { id: tab.id, title: tab.title ?? "" };
    },
    async surveyTab(tabId) {
      const reply: unknown = await chrome.runtime.sendMessage({ kind: SURVEY_TAB_KIND, tab_id: tabId });
      if (!isSurveyTabReply(reply)) throw new Error("the background gave no valid answer to survey_tab");
      return reply;
    },
    async handOffTab(request) {
      // Строгий разбор фона принимает только присутствующие поля, поэтому пустые не отправляем.
      const message = {
        kind: HAND_OFF_TAB_KIND,
        tab_id: request.tabId,
        ...(request.video === undefined ? {} : { video: request.video }),
        ...(request.consentedLocalHost === undefined ? {} : { consented_local_host: request.consentedLocalHost }),
      };
      const outcome: unknown = await chrome.runtime.sendMessage(message);
      if (!isHandOffTabOutcome(outcome)) throw new Error("the background gave no valid answer to hand_off_tab");
      return outcome;
    },
  };
}
