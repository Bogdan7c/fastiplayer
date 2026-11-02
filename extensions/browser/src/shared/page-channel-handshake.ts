// Рукопожатие участка 1 (MAIN-мир ⇄ изолированный мир) — договор пакетов A1 и B сессии 09a.
//
// Изолированный мир на document_start создаёт MessageChannel, оставляет себе port1 и передаёт port2
// MAIN-миру одним `window.postMessage(HANDSHAKE_MESSAGE, "*", [port2])`. MAIN-мир слушает `message`
// на window в фазе захвата (его слушатель зарегистрирован раньше любых скриптов страницы), принимает
// ТОЛЬКО первое такое сообщение с `event.source === window` и ровно одним портом и зовёт
// `stopImmediatePropagation()` — обработчики страницы его не видят и порт не получают.
// Дальше весь обмен идёт по личному порту: страница его не видит и не может подделать.
//
// Почему не обмен через window.postMessage: каждое наше сообщение (и каждый кусок байтов) получали бы
// все обработчики `message` страницы — лишняя работа на её потоке и риск сломать чужую логику.

/** Поле-метка рукопожатия. Значение — номер версии внутреннего договора. */
export const HANDSHAKE_MARKER_FIELD = "fastiplayerBridgePageChannel";

/** Версия внутреннего договора участка 1 (меняется вместе с handoff-contract.ts). */
export const HANDSHAKE_VERSION = 1;

/** Сообщение рукопожатия (порт передаётся отдельно, в списке transfer). */
export const HANDSHAKE_MESSAGE: Readonly<Record<string, number>> = Object.freeze({
  [HANDSHAKE_MARKER_FIELD]: HANDSHAKE_VERSION,
});
