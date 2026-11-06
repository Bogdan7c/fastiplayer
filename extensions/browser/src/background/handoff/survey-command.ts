// Приём внутренней команды «опроси вкладку N» (сессия 10b) через runtime.onMessage.
//
// Правило отправителя то же, что у `hand_off_tab` (hand-off-command.ts): только страницы самого
// экстеншена. Контент-скрипт и страницы сайтов сюда не проходят.

import { SURVEY_TAB_KIND, type SurveyTabReply, type SurveyTabRequest } from "../../shared/survey-contract.ts";
import { isOwnExtensionPage, type CommandSender, type ExtensionIdentityForSenders } from "./hand-off-command.ts";

/** Строгая форма: ровно `kind` и целый неотрицательный `tab_id`. */
export function parseSurveyTabRequest(message: unknown): SurveyTabRequest | undefined {
  if (typeof message !== "object" || message === null || Array.isArray(message)) return undefined;
  const record = message as Record<string, unknown>;
  const tabId = record["tab_id"];
  const valid =
    record["kind"] === SURVEY_TAB_KIND &&
    Object.keys(record).length === 2 &&
    typeof tabId === "number" &&
    Number.isSafeInteger(tabId) &&
    tabId >= 0;
  return valid ? { kind: SURVEY_TAB_KIND, tab_id: tabId } : undefined;
}

/** Слушатель runtime.onMessage: true — ответ придёт асинхронно. */
export type SurveyMessageListener = (message: unknown, sender: CommandSender, sendResponse: (reply: SurveyTabReply) => void) => boolean;

export function createSurveyTabCommandListener(
  identity: ExtensionIdentityForSenders,
  surveyTab: (tabId: number) => Promise<SurveyTabReply>,
): SurveyMessageListener {
  return (message, sender, sendResponse) => {
    // Чужие сообщения не наши: не отвечаем, не держим канал открытым.
    if (!isOwnExtensionPage(sender, identity)) return false;
    const request = parseSurveyTabRequest(message);
    if (request === undefined) return false;
    surveyTab(request.tab_id).then(sendResponse, () => sendResponse({ kind: "unavailable", reason: "survey_failed" }));
    return true;
  };
}
