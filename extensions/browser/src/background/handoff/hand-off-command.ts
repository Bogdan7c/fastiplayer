// Приём внутренней команды «отдать вкладку N» (решение 175) через runtime.onMessage.
//
// Принимаем ТОЛЬКО от страниц самого экстеншена: свой ID и адрес внутри экстеншена. Контент-скрипт
// (даже наш) шлёт с адресом страницы сайта и сюда не проходит, а страница сайта runtime-сообщений
// экстеншену слать не может вовсе (externally_connectable не объявлен). Тестовых дыр нет.
// Поэтому согласие на локальный адрес `consented_local_host` (решение 199) подделать некому: разбор
// видит только сообщения страниц экстеншена, а попап 10 ставит поле после вопроса человеку.
//
// Вкладка-отправитель (`sender.tab`) НЕ запрещена: служебная страница экстеншена, открытая во
// вкладке (её открывает робот стенда; страница настроек тоже открывается во вкладке), остаётся
// страницей экстеншена — её адрес начинается с адреса экстеншена.

import { HAND_OFF_TAB_KIND, type HandOffTabOutcome, type HandOffTabRequest, type HeldVideoReply, type HeldVideoReport, type VideoRef } from "../../shared/handoff-contract.ts";
import { isDocumentTag, isHostName, parseHeldVideoReport } from "../../shared/page-message-validation.ts";
import { NO_LOCAL_ADDRESS_CONSENT, type LocalAddressConsent } from "./file-address-guard.ts";

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

/** Целое неотрицательное число (номера фрейма, вкладки и элемента). */
function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Строгая ссылка на видео: ровно `frame_id`, `document` (метка документа) и `element`. */
export function parseVideoRef(value: unknown): VideoRef | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const record = value as Record<string, unknown>;
  const valid =
    Object.keys(record).length === 3 &&
    isNonNegativeSafeInteger(record["frame_id"]) &&
    isDocumentTag(record["document"]) &&
    isNonNegativeSafeInteger(record["element"]);
  return valid ? { frame_id: record["frame_id"] as number, document: record["document"] as string, element: record["element"] as number } : undefined;
}

/**
 * Строгая форма команды: `kind`, целый неотрицательный `tab_id`, необязательное `consented_local_host`
 * (имя хоста) и необязательное `video` (ссылка на видео). Любое другое поле или значение — не наша команда.
 */
export function parseHandOffTabRequest(message: unknown): HandOffTabRequest | undefined {
  if (typeof message !== "object" || message === null || Array.isArray(message)) return undefined;
  const record = message as Record<string, unknown>;
  const hasConsent = Object.hasOwn(record, "consented_local_host");
  const hasVideo = Object.hasOwn(record, "video");
  const consentedHost = record["consented_local_host"];
  const video = hasVideo ? parseVideoRef(record["video"]) : undefined;
  const valid =
    record["kind"] === HAND_OFF_TAB_KIND &&
    Object.keys(record).length === 2 + (hasConsent ? 1 : 0) + (hasVideo ? 1 : 0) &&
    (!hasConsent || isHostName(consentedHost)) &&
    (!hasVideo || video !== undefined) &&
    Object.hasOwn(record, "tab_id") &&
    isNonNegativeSafeInteger(record["tab_id"]);
  if (!valid) return undefined;
  return {
    kind: HAND_OFF_TAB_KIND,
    tab_id: record["tab_id"] as number,
    ...(hasConsent ? { consented_local_host: consentedHost as string } : {}),
    ...(video !== undefined ? { video } : {}),
  };
}

/** Отправитель донесения из контент-скрипта: вкладку и фрейм называет браузер, а не страница. */
export interface FrameSender extends CommandSender {
  readonly tab?: { readonly id?: number | undefined } | undefined;
  readonly frameId?: number | undefined;
}

/**
 * Слушатель донесений сторожа следующего видео. Принимаем только свой контент-скрипт. Страница сайта
 * `runtime`-сообщений слать не может, а её данные в донесении проверяются заново.
 */
export function createHeldVideoListener(
  extensionId: string,
  onHeld: (tabId: number, frameId: number, report: HeldVideoReport) => HeldVideoReply,
): (message: unknown, sender: FrameSender, sendResponse: (reply: HeldVideoReply) => void) => boolean {
  return (message, sender, sendResponse) => {
    const tabId = sender.tab?.id;
    if (sender.id !== extensionId || tabId === undefined || sender.frameId === undefined) return false;
    const report = parseHeldVideoReport(message);
    if (report === undefined) return false;
    sendResponse(onHeld(tabId, sender.frameId, report));
    return false;
  };
}

/** Слушатель runtime.onMessage: true — ответ придёт асинхронно (sendResponse позже). */
export type RuntimeMessageListener = (
  message: unknown,
  sender: CommandSender,
  sendResponse: (response: HandOffTabOutcome) => void,
) => boolean;

export function createHandOffCommandListener(
  identity: ExtensionIdentityForSenders,
  handOffTab: (tabId: number, consent: LocalAddressConsent, video: VideoRef | undefined) => Promise<HandOffTabOutcome>,
): RuntimeMessageListener {
  return (message, sender, sendResponse) => {
    // Чужие сообщения (и наша команда от не-страницы экстеншена) не наши: не отвечаем, не держим канал.
    if (!isOwnExtensionPage(sender, identity)) return false;
    const request = parseHandOffTabRequest(message);
    if (request === undefined) return false;
    const consent: LocalAddressConsent =
      request.consented_local_host === undefined ? NO_LOCAL_ADDRESS_CONSENT : { kind: "given_for", host: request.consented_local_host };
    handOffTab(request.tab_id, consent, request.video).then(sendResponse, (error: unknown) =>
      sendResponse({ kind: "failed", reason: `internal:${String(error)}` }),
    );
    return true;
  };
}
