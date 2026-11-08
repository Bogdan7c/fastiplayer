// Договор предложения «Ask» режима 2 (сессия 12b, решения 255–258): кто кому что говорит и как это проверяется.
//
// Сайт помечен `ask` (или новый сайт при `unknown_site_rule: ask`), человек сам нажал play на фильме со звуком.
// Фон отвечает сторожу не «берём» и не «отпусти», а «спроси» (`ask_viewer`, handoff-contract.ts). Дальше:
//
//  1. изолированный мир → MAIN-мир (личный порт): `hold_for_viewer_offer` — держать видео под тенью, модель тени
//     на паузе (страница видит обычную паузу), якорь сообщает изолированному миру, над каким элементом рисовать
//     предложение. Страховочного срока у такого удержания нет: предложение висит, пока человек не выберет
//     (решение 256), или пока страница сама не уйдёт;
//  2. изолированный мир → фон (`runtime.sendMessage`): выбор человека `viewer_offer_choice`. Рождается только
//     на настоящий клик по кнопке нашего закрытого слоя, поэтому страница его подделать не может. Вкладку,
//     фрейм и адрес сайта фон берёт из отправителя, а не из сообщения: правило пишется на сайт вкладки;
//  3. фон → изолированный мир: ответ на выбор (`hand_off` / `play_here` / `no_offer`) и, когда на той же вкладке
//     встаёт новое предложение, просьба снять старое `withdraw_viewer_offer_request` (одно предложение на вкладку);
//  4. изолированный мир → MAIN-мир: `withdraw_viewer_offer` — снять тень, видео остаётся на паузе. «Play here»
//     идёт обычным `release_held_video`: сторож сам знает, что видео держалось под предложением, и запускает его.
//
// Все формы проверяются строго (лишнее поле — не наше сообщение), как остальные сообщения миров.

import { MAX_JSON_SAFE_INTEGER } from "../protocol/limits.ts";
import type { DocumentTag, ElementKey } from "./handoff-contract.ts";
import { isDocumentTag } from "./page-message-validation.ts";

// ---------------------------------------------------------------------------------------------
// Участок 1: изолированный мир → MAIN-мир (личный порт).
// ---------------------------------------------------------------------------------------------

/** «Держи задержанное видео и спроси человека»: модель тени — на паузу, якорю — сигнал предложения. */
export interface HoldForViewerOfferCommand {
  readonly kind: "hold_for_viewer_offer";
  readonly element: ElementKey;
}

/** «Сними предложение»: тень снимается, видео остаётся на паузе (на той же вкладке встало новое предложение). */
export interface WithdrawViewerOfferCommand {
  readonly kind: "withdraw_viewer_offer";
  readonly element: ElementKey;
}

/** Команды предложения, которые MAIN-мир получает по личному порту (принимает их сторож следующего видео). */
export type ViewerOfferPageCommand = HoldForViewerOfferCommand | WithdrawViewerOfferCommand;

// ---------------------------------------------------------------------------------------------
// Участок 2: изолированный мир ↔ фон.
// ---------------------------------------------------------------------------------------------

/**
 * Что человек нажал на предложении:
 * - `fastiplayer` — «Play in Fastiplayer»: обычная передача, правило не меняется;
 * - `here` — «Play here»: видео играет в браузере, правило не меняется;
 * - `always` / `never` — правило сайта и сразу действие: `always` — в плеер, `never` — здесь (решение 255).
 */
export const VIEWER_OFFER_CHOICES = ["fastiplayer", "here", "always", "never"] as const;
export type ViewerOfferChoice = (typeof VIEWER_OFFER_CHOICES)[number];

/** Вид сообщения выбора. */
export const VIEWER_OFFER_CHOICE_KIND = "viewer_offer_choice";

/** Выбор человека. `document` и `element` — как в донесении о задержанном видео (по ним фон строит ссылку на видео). */
export interface ViewerOfferChoiceMessage {
  readonly kind: typeof VIEWER_OFFER_CHOICE_KIND;
  readonly document: DocumentTag;
  readonly element: ElementKey;
  readonly choice: ViewerOfferChoice;
}

/**
 * Ответ фона на выбор:
 * - `hand_off` — передача началась; `begin_handoff` придёт после запуска плеера, а не вышло — фон сам пришлёт
 *   «отпусти» (`release_held_video_request`), как у обычной передачи режима 2;
 * - `play_here` — отпустить видео: оно заиграет в браузере;
 * - `no_offer` — фон предложения не узнал (сайт больше не `ask`, правило поменяли из попапа): видео
 *   отпускается и играет здесь — человек нажимал play, значит, хотел смотреть.
 */
export const VIEWER_OFFER_ANSWERS = ["hand_off", "play_here", "no_offer"] as const;
export type ViewerOfferAnswer = (typeof VIEWER_OFFER_ANSWERS)[number];

/** Вид просьбы фона одному фрейму: «сними предложение с этого элемента, видео пусть стоит на паузе». */
export const WITHDRAW_VIEWER_OFFER_REQUEST_KIND = "withdraw_viewer_offer_request";

export interface WithdrawViewerOfferRequest {
  readonly kind: typeof WITHDRAW_VIEWER_OFFER_REQUEST_KIND;
  readonly element: ElementKey;
}

// ---------------------------------------------------------------------------------------------
// Проверка формы.
// ---------------------------------------------------------------------------------------------

type PlainRecord = Readonly<Record<string, unknown>>;

/** Объект ровно с этими полями и видом `kind`; иначе `null`. */
function exactRecord(value: unknown, kind: string, keys: readonly string[]): PlainRecord | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as PlainRecord;
  const ownKeys = Object.keys(record);
  if (ownKeys.length !== keys.length || !keys.every((key) => Object.hasOwn(record, key))) return null;
  return record["kind"] === kind ? record : null;
}

function isElementKey(value: unknown): value is ElementKey {
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= MAX_JSON_SAFE_INTEGER;
}

function isOneOf<T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value);
}

/** Команда предложения по личному порту; `undefined` — не она или не по форме. */
export function parseViewerOfferPageCommand(value: unknown): ViewerOfferPageCommand | undefined {
  for (const kind of ["hold_for_viewer_offer", "withdraw_viewer_offer"] as const) {
    const record = exactRecord(value, kind, ["kind", "element"]);
    if (record !== null) return isElementKey(record["element"]) ? { kind, element: record["element"] } : undefined;
  }
  return undefined;
}

/** Выбор человека, как его принимает фон; `undefined` — не по форме. */
export function parseViewerOfferChoiceMessage(value: unknown): ViewerOfferChoiceMessage | undefined {
  const record = exactRecord(value, VIEWER_OFFER_CHOICE_KIND, ["kind", "document", "element", "choice"]);
  if (record === null) return undefined;
  const { document, element, choice } = record;
  if (!isDocumentTag(document) || !isElementKey(element) || !isOneOf(choice, VIEWER_OFFER_CHOICES)) return undefined;
  return { kind: VIEWER_OFFER_CHOICE_KIND, document, element, choice };
}

/** Ответ фона на выбор; не из договора — `undefined` (изолированный мир тогда отпускает видео). */
export function parseViewerOfferAnswer(value: unknown): ViewerOfferAnswer | undefined {
  return isOneOf(value, VIEWER_OFFER_ANSWERS) ? value : undefined;
}

/** Просьба фона снять предложение; `undefined` — не она или не по форме. */
export function parseWithdrawViewerOfferRequest(value: unknown): WithdrawViewerOfferRequest | undefined {
  const record = exactRecord(value, WITHDRAW_VIEWER_OFFER_REQUEST_KIND, ["kind", "element"]);
  if (record === null || !isElementKey(record["element"])) return undefined;
  return { kind: WITHDRAW_VIEWER_OFFER_REQUEST_KIND, element: record["element"] };
}
