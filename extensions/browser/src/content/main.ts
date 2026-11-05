// Изолированный контент-скрипт (ISOLATED world), document_start, все фреймы.
// Здесь только сборка: личный канал с MAIN-миром, пограничник, порт фона, функция выбора видео.
//
// До передачи скрипт ничего не шлёт фону и не будит его: порт участка 2 открывает сам фон
// (tabs.connect) в момент «отдать вкладку», а описание видео фон запрашивает через executeScript.
import { browserApi } from "../shared/browser-api.ts";
import { HANDOFF_PORT_NAME } from "../shared/handoff-contract.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME } from "../shared/video-candidates-probe.ts";
import { openPageChannel, type PageChannelPort } from "./page-channel.ts";
import { createContentRelay, type ContentRelay } from "./relay.ts";

/** Сериализация непрозрачного происхождения (`about:blank`, `data:`). */
const OPAQUE_ORIGIN = "null";

let debugReported = false;

// Пограничник создаётся до канала (канал сразу передаёт ему сообщения), отправка — через ссылку.
let sendToPageWorld: (message: unknown) => void = () => {};
const relay: ContentRelay = createContentRelay({
  sendToPage: (message) => sendToPageWorld(message),
  timers: {
    setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
    clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  },
  // Журнала в продукте нет: одна строка за жизнь документа, только для разработчика в консоли.
  debugOnce: (message) => {
    if (debugReported) return;
    debugReported = true;
    console.debug(message);
  },
  now: () => performance.now(),
  // Читается в изолированном мире: страница не может подменить то, что документ говорит о себе.
  ownDocument: () => ({
    hostname: location.hostname,
    origin: location.origin === OPAQUE_ORIGIN ? null : location.origin,
  }),
});

const channel = openPageChannel(
  {
    createChannel: () => {
      const messageChannel = new MessageChannel();
      return { port1: messageChannel.port1 as PageChannelPort, port2: messageChannel.port2 };
    },
    postToOwnWindow: (message, transfer) => window.postMessage(message, "*", transfer),
    rendezvous: window,
    createCancelableEvent: (type) => new Event(type, { cancelable: true }),
  },
  (message) => relay.handlePageMessage(message),
);
sendToPageWorld = (message) => channel.send(message);

// Порт участка 2 открывает только наш фон; чужие экстеншены к нашим контент-скриптам не подключаются,
// но имя и отправителя всё равно проверяем.
browserApi.runtime.onConnect.addListener((port) => {
  const senderId = port.sender?.id;
  if (port.name !== HANDOFF_PORT_NAME || (senderId !== undefined && senderId !== browserApi.runtime.id)) return;
  relay.acceptBackgroundPort(port);
});

// Функция для executeScript фона живёт в глобале ИЗОЛИРОВАННОГО мира: странице она не видна.
Object.defineProperty(globalThis, VIDEO_CANDIDATES_FUNCTION_NAME, {
  value: () => relay.describeVideoCandidates(),
  configurable: false,
  enumerable: false,
  writable: false,
});
