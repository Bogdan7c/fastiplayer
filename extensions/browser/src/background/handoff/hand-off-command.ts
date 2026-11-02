// Приём внутренней команды «отдать вкладку N» (решение 175) через runtime.onMessage.
//
// Принимаем ТОЛЬКО от страниц самого экстеншена: свой ID и адрес внутри экстеншена. Контент-скрипт
// (даже наш) шлёт с адресом страницы сайта и сюда не проходит, а страница сайта runtime-сообщений
// экстеншену слать не может вовсе (externally_connectable не объявлен). Тестовых дыр нет.
//
// Вкладка-отправитель (`sender.tab`) НЕ запрещена: служебная страница экстеншена, открытая во
// вкладке (её открывает робот стенда; страница настроек тоже открывается во вкладке), остаётся
// страницей экстеншена — её адрес начинается с адреса экстеншена.

import { HAND_OFF_TAB_KIND, type HandOffTabOutcome, type HandOffTabRequest } from "../../shared/handoff-contract.ts";

/** Отправитель сообщения (runtime.MessageSender) в той мере, в какой он нужен проверке. */
export interface CommandSender {
  readonly id?: string | undefined;
  readonly url?: string | undefined;
}

export interface ExtensionIdentityForSenders {
  /** `runtime.id`. */
  readonly extensionId: string;
  /** `runtime.getURL("")`: «chrome-extension://<id>/» или «moz-extension://<uuid>/». */
  readonly extensionBaseUrl: string;
}

/** Отправитель — страница этого экстеншена (попап, служебная страница, настройки). */
export function isOwnExtensionPage(sender: CommandSender, identity: ExtensionIdentityForSenders): boolean {
  return (
    sender.id === identity.extensionId &&
    identity.extensionBaseUrl.length > 0 &&
    typeof sender.url === "string" &&
    sender.url.startsWith(identity.extensionBaseUrl)
  );
}

/** Строгая форма команды: ровно `kind` и целый неотрицательный `tab_id`. */
export function parseHandOffTabRequest(message: unknown): HandOffTabRequest | undefined {
  if (typeof message !== "object" || message === null || Array.isArray(message)) return undefined;
  const record = message as Record<string, unknown>;
  const keys = Object.keys(record);
  const tabId = record["tab_id"];
  const valid =
    record["kind"] === HAND_OFF_TAB_KIND &&
    keys.length === 2 &&
    Object.hasOwn(record, "tab_id") &&
    typeof tabId === "number" &&
    Number.isSafeInteger(tabId) &&
    tabId >= 0;
  return valid ? { kind: HAND_OFF_TAB_KIND, tab_id: tabId as number } : undefined;
}

/** Слушатель runtime.onMessage: true — ответ придёт асинхронно (sendResponse позже). */
export type RuntimeMessageListener = (
  message: unknown,
  sender: CommandSender,
  sendResponse: (response: HandOffTabOutcome) => void,
) => boolean;

export function createHandOffCommandListener(
  identity: ExtensionIdentityForSenders,
  handOffTab: (tabId: number) => Promise<HandOffTabOutcome>,
): RuntimeMessageListener {
  return (message, sender, sendResponse) => {
    // Чужие сообщения (и наша команда от не-страницы экстеншена) не наши: не отвечаем, не держим канал.
    if (!isOwnExtensionPage(sender, identity)) return false;
    const request = parseHandOffTabRequest(message);
    if (request === undefined) return false;
    handOffTab(request.tab_id).then(sendResponse, (error: unknown) =>
      sendResponse({ kind: "failed", reason: `internal:${String(error)}` }),
    );
    return true;
  };
}
