// Приём команд позднего входа через runtime.onMessage (сессия 10d). Правило отправителя то же, что у
// `hand_off_tab`: только страницы самого экстеншена, поэтому перезагрузить вкладку страница сайта не может.

import { LATE_ENTRY_STATE_KIND, RELOAD_AND_OPEN_KIND, type LateEntryStateReply, type LateEntryStateRequest, type ReloadAndOpenRequest } from "../../shared/late-entry-contract.ts";
import { isOwnExtensionPage, type CommandSender, type ExtensionIdentityForSenders } from "../handoff/hand-off-command.ts";
import type { LateEntry } from "./late-entry.ts";

/** Строгая форма: ровно `kind` и целый неотрицательный `tab_id`. */
export function parseLateEntryRequest(message: unknown): ReloadAndOpenRequest | LateEntryStateRequest | undefined {
  if (typeof message !== "object" || message === null || Array.isArray(message)) return undefined;
  const record = message as Record<string, unknown>;
  const { kind, tab_id: tabId } = record;
  if (Object.keys(record).length !== 2 || typeof tabId !== "number" || !Number.isSafeInteger(tabId) || tabId < 0) return undefined;
  if (kind === RELOAD_AND_OPEN_KIND) return { kind, tab_id: tabId };
  return kind === LATE_ENTRY_STATE_KIND ? { kind, tab_id: tabId } : undefined;
}

/** Слушатель runtime.onMessage. Ответ всегда синхронный: работу ведёт фон, попап её не ждёт. */
export function createLateEntryCommandListener(
  identity: ExtensionIdentityForSenders,
  lateEntry: Pick<LateEntry, "start" | "isRunning">,
): (message: unknown, sender: CommandSender, sendResponse: (reply: LateEntryStateReply) => void) => boolean {
  return (message, sender, sendResponse) => {
    if (!isOwnExtensionPage(sender, identity)) return false;
    const request = parseLateEntryRequest(message);
    if (request === undefined) return false;
    if (request.kind === RELOAD_AND_OPEN_KIND) lateEntry.start(request.tab_id);
    sendResponse({ running: lateEntry.isRunning(request.tab_id) });
    return false;
  };
}
