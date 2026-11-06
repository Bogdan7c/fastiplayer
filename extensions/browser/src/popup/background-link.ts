// Настоящие зависимости попапа: активная вкладка и две команды фону. Ответ фона проверяем по форме:
// отсутствующий или кривой ответ (фон не слушает, отвечает чужой слушатель) — отклонение, а не «тихая» ошибка.

import { HAND_OFF_TAB_KIND, type HandOffTabOutcome } from "../shared/handoff-contract.ts";
import { SURVEY_TAB_KIND, type SurveyTabReply } from "../shared/survey-contract.ts";
import { LATE_ENTRY_STATE_KIND, RELOAD_AND_OPEN_KIND, type LateEntryStateReply } from "../shared/late-entry-contract.ts";
import type { ActiveTab, HandOffRequest } from "./popup-flow.ts";

const OUTCOME_KINDS: readonly HandOffTabOutcome["kind"][] = [
  "started",
  "no_video",
  "not_supported_yet",
  "protected",
  "reload_needed",
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
  if (!isRecord(value) || !OUTCOME_KINDS.some((kind) => kind === value["kind"])) return false;
  // Без окна плеера (старый фон?) попап не знает, закрываться или нет: такой ответ считаем кривым.
  return value["kind"] !== "started" || value["player_window"] === "new" || value["player_window"] === "already_open";
}

/** Ответ на `survey_tab` — известный вид ответа. */
export function isSurveyTabReply(value: unknown): value is SurveyTabReply {
  return isRecord(value) && (value["kind"] === "surveyed" || value["kind"] === "unavailable");
}

/** Ответ на команды позднего входа. */
export function isLateEntryStateReply(value: unknown): value is LateEntryStateReply {
  return isRecord(value) && typeof value["running"] === "boolean";
}

/** Команда позднего входа; ответ проверен по форме, как и у остальных команд. */
async function askLateEntry(kind: typeof LATE_ENTRY_STATE_KIND | typeof RELOAD_AND_OPEN_KIND, tabId: number): Promise<boolean> {
  const reply: unknown = await chrome.runtime.sendMessage({ kind, tab_id: tabId });
  if (!isLateEntryStateReply(reply)) throw new Error(`the background gave no valid answer to ${kind}`);
  return reply.running;
}

export interface BackgroundLink {
  findActiveTab(): Promise<ActiveTab | undefined>;
  surveyTab(tabId: number): Promise<SurveyTabReply>;
  handOffTab(request: HandOffRequest): Promise<HandOffTabOutcome>;
  isLateEntryRunning(tabId: number): Promise<boolean>;
  reloadAndOpen(tabId: number): Promise<void>;
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
    isLateEntryRunning: (tabId) => askLateEntry(LATE_ENTRY_STATE_KIND, tabId),
    reloadAndOpen: async (tabId) => void (await askLateEntry(RELOAD_AND_OPEN_KIND, tabId)),
  };
}
