// Участок 1, сторона изолированного мира: личный MessagePort с MAIN-миром (договор
// src/shared/page-channel-handshake.ts).
//
// На document_start изолированный мир создаёт MessageChannel, оставляет себе port1 и передаёт port2
// MAIN-миру одним `window.postMessage`. Дальше весь обмен — по порту: обработчики `message` страницы
// не видят ни сообщений, ни байтов.
//
// Firefox (живая разведка 09a): сообщение, пришедшее по порту, живёт в отсеке СТРАНИЦЫ и видно
// изолированному миру через Xray: `instanceof Uint8Array` ложно, побайтовое чтение ~275 мс/МиБ.
// `structuredClone` в изолированном мире делает родную копию (~0,5 мс/МиБ) — её и проверяем.
// В Chromium сообщение сразу родное, копия не нужна.
//
// Почему рукопожатие одно и без повторов. Оба скрипта (изолированный и MAIN) стоят на
// document_start и исполняются браузером синхронно, один за другим, до любого скрипта страницы.
// `window.postMessage` не доставляет сообщение сразу: событие `message` ставится отдельной задачей
// в очередь цикла событий, а она запускается только после того, как оба наших скрипта отработали.
// Поэтому слушатель MAIN-мира уже стоит к моменту доставки, даже если изолированный скрипт
// исполнился первым (так и есть: в манифесте content.js идёт раньше page.js). Живая разведка 09a
// проверяла ровно этот порядок (изолированный первым) — Chromium 156 и Firefox 158, ~15 запусков
// на обычной странице: рукопожатие дошло каждый раз, случая «MAIN не успел» не было.
// Повтор был бы вреден: второе сообщение с меткой увидели бы обработчики `message` страницы
// (MAIN-мир принимает и прячет только первое).

import { HANDSHAKE_MESSAGE } from "../shared/page-channel-handshake.ts";

/** Порт канала в той мере, в какой он нам нужен (MessagePort или фейк в тестах). */
export interface PageChannelPort {
  postMessage(message: unknown): void;
  onmessage: ((event: { readonly data: unknown }) => void) | null;
}

/** Окружение изолированного мира (в продукте — настоящий window, в тестах — фейки). */
export interface PageChannelEnvironment {
  /** Новый MessageChannel. */
  createChannel(): { readonly port1: PageChannelPort; readonly port2: Transferable };
  /** `window.postMessage(message, "*", transfer)` своего окна. */
  postToOwnWindow(message: unknown, transfer: Transferable[]): void;
}

/** Отправка MAIN-миру по личному порту. */
export interface PageChannel {
  send(message: unknown): void;
}

/**
 * Значение, пришедшее из MAIN-мира, в виде объекта ЭТОГО мира. Чужой отсек (Firefox) узнаётся по
 * тому, что объект не наследует `Object` этого мира; тогда делаем родную структурную копию.
 * `undefined` — копия не удалась (страница подсунула то, что не клонируется): это мусор.
 */
export function adoptPageValue(raw: unknown): unknown {
  if (typeof raw !== "object" || raw === null || raw instanceof Object) return raw;
  try {
    return structuredClone(raw);
  } catch {
    return undefined;
  }
}

/**
 * Открывает канал: создаёт порт, вешает приёмник и передаёт второй конец MAIN-миру.
 * `onPageMessage` получает каждое сообщение уже «усыновлённым» (`adoptPageValue`), но НЕ проверенным.
 */
export function openPageChannel(environment: PageChannelEnvironment, onPageMessage: (message: unknown) => void): PageChannel {
  const channel = environment.createChannel();
  const ownPort = channel.port1;
  // Присвоение onmessage само запускает порт (start()), ранние сообщения не теряются: до передачи
  // порта MAIN-миру никто в него не пишет.
  ownPort.onmessage = (event) => onPageMessage(adoptPageValue(event.data));
  environment.postToOwnWindow(HANDSHAKE_MESSAGE, [channel.port2]);
  return { send: (message) => ownPort.postMessage(message) };
}
