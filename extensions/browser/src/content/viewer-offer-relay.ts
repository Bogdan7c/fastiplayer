// Пограничник предложения «Ask» (сессия 12b; договор — src/shared/viewer-offer-contract.ts), изолированный мир.
//
// Владеет одним знанием: «какое задержанное видео ЭТОГО документа сейчас под предложением». Ключ элемента и
// метку документа он помнит сам (из донесения сторожа), поэтому панель на странице передаёт только «что нажато»,
// а не «над чем»: подсунуть чужой элемент ей нечем.
//
// Порядок:
//  1. фон ответил сторожу `ask_viewer` -> `offerStood`: MAIN-миру `hold_for_viewer_offer`;
//  2. человек нажал кнопку панели (настоящий клик проверяет панель) -> `answer`: выбор уходит фону, ответ
//     переводится MAIN-миру. «В плеер» ждёт `begin_handoff`, а если фон умер до него — видео отпускается через
//     `NEW_SESSION_ADOPT_INSURANCE_MS`, как у обычной передачи режима 2. Всё остальное (здесь, фон не узнал
//     предложение, фон недоступен) — видео отпускается и играет здесь: человек нажимал play, значит, хотел смотреть;
//  3. фон снял предложение (на вкладке встало новое) -> `withdraw`: MAIN-миру `withdraw_viewer_offer`.

import { NEW_SESSION_ADOPT_INSURANCE_MS, type DocumentTag, type ElementKey, type NextVideoCommand } from "../shared/handoff-contract.ts";
import {
  parseViewerOfferAnswer,
  VIEWER_OFFER_CHOICE_KIND,
  type ViewerOfferChoice,
  type ViewerOfferChoiceMessage,
  type ViewerOfferPageCommand,
} from "../shared/viewer-offer-contract.ts";
import type { RelayTimers } from "./relay.ts";

/**
 * Что стало с выбором:
 * - `hand_off` — фон начал передачу, видео ждёт плеер;
 * - `played_here` — видео отпущено и играет в браузере (выбор «здесь» или фон не узнал предложение);
 * - `no_offer` — предложения в этом документе нет (уже выбрано, снято, ушло): нечего отвечать;
 * - `send_failed` — фон недоступен: видео отпущено и играет здесь, причина — в журнал разработчика.
 */
export type ViewerOfferDelivery = "hand_off" | "played_here" | "no_offer" | "send_failed";

export interface ViewerOfferRelayDependencies {
  /** Отправка MAIN-миру по личному порту участка 1. */
  readonly sendToPage: (message: ViewerOfferPageCommand | NextVideoCommand) => void;
  /** Выбор фону (`runtime.sendMessage`); ответ проверяется здесь. */
  readonly sendChoice: (message: ViewerOfferChoiceMessage) => Promise<unknown>;
  readonly timers: RelayTimers;
  readonly note: (message: string, error: unknown) => void;
}

export interface ViewerOfferRelay {
  /** Фон ответил «спроси» на донесение об этом видео. */
  offerStood(element: ElementKey, document: DocumentTag): void;
  /** Человек нажал кнопку панели. Не отклоняется. */
  answer(choice: ViewerOfferChoice): Promise<ViewerOfferDelivery>;
  /** Фон просит снять предложение с этого элемента (чужой ключ — ничего). */
  withdraw(element: ElementKey): void;
}

/** Предложение, которое висит в этом документе. */
interface PendingOffer {
  readonly element: ElementKey;
  readonly document: DocumentTag;
}

export function createViewerOfferRelay(dependencies: ViewerOfferRelayDependencies): ViewerOfferRelay {
  let pending: PendingOffer | null = null;

  const release = (element: ElementKey): void => dependencies.sendToPage({ kind: "release_held_video", element });

  async function answer(choice: ViewerOfferChoice): Promise<ViewerOfferDelivery> {
    const offer = pending;
    if (offer === null) return "no_offer";
    // Второй клик (или кнопка соседа) по тому же предложению уже ничего не решает.
    pending = null;
    let reply: unknown;
    try {
      reply = await dependencies.sendChoice({ kind: VIEWER_OFFER_CHOICE_KIND, document: offer.document, element: offer.element, choice });
    } catch (error) {
      dependencies.note("the viewer offer choice did not reach the background, the video plays here", error);
      release(offer.element);
      return "send_failed";
    }
    if (parseViewerOfferAnswer(reply) === "hand_off") {
      // Забранное видео этой командой не отпустить (для него она ничего не значит), поэтому страховка ставится всегда.
      dependencies.timers.setTimeout(() => release(offer.element), NEW_SESSION_ADOPT_INSURANCE_MS);
      return "hand_off";
    }
    release(offer.element);
    return "played_here";
  }

  return {
    offerStood(element, document) {
      pending = { element, document };
      dependencies.sendToPage({ kind: "hold_for_viewer_offer", element });
    },
    answer,
    withdraw(element) {
      if (pending?.element === element) pending = null;
      // MAIN-мир сам проверит, висит ли предложение на этом элементе: чужой ключ там тоже ничего не делает.
      dependencies.sendToPage({ kind: "withdraw_viewer_offer", element });
    },
  };
}
