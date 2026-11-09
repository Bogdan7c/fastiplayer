// Просьбы страницы настроек к памяти выбора (сессия 14b2): «сколько сериалов помнишь» и «Forget remembered
// choices». Стирает фон, а не сама страница: у памяти один владелец и одна очередь (`choice-memory-store.ts`),
// поэтому «Forget» не может разминуться с записью, которая как раз идёт.
//
// Правило отправителя то же, что у `hand_off_tab`: только страницы самого экстеншена.

import { parseChoiceMemoryRequest, type ChoiceMemoryReply } from "../../shared/choice-memory-contract.ts";
import { isOwnExtensionPage, type CommandSender, type ExtensionIdentityForSenders } from "../handoff/hand-off-command.ts";
import type { ChoiceMemoryStore } from "./choice-memory-store.ts";

/** Слушатель runtime.onMessage: true — ответ придёт асинхронно. */
export type ChoiceMemoryMessageListener = (message: unknown, sender: CommandSender, sendResponse: (reply: ChoiceMemoryReply) => void) => boolean;

export function createChoiceMemoryCommandListener(identity: ExtensionIdentityForSenders, memory: ChoiceMemoryStore): ChoiceMemoryMessageListener {
  return (message, sender, sendResponse) => {
    // Чужие сообщения не наши: не отвечаем, не держим канал открытым.
    if (!isOwnExtensionPage(sender, identity)) return false;
    const request = parseChoiceMemoryRequest(message);
    if (request === undefined) return false;
    const answer: Promise<ChoiceMemoryReply> =
      request.kind === "forget_remembered_choices"
        ? memory.forget().then(() => ({ kind: "forgotten" }))
        : memory.count().then((series) => ({ kind: "count", series }));
    // Сбой хранилища — честный ответ странице настроек, она покажет его текстом.
    answer.then(sendResponse, (error: unknown) => sendResponse({ kind: "failed", reason: String(error) }));
    return true;
  };
}
