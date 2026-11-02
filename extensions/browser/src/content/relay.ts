// Изолированный мир — «пограничник» между MAIN-миром страницы и фоном (сессия 09a).
//
// Владеет:
// - проверкой формы КАЖДОГО сообщения MAIN-мира (src/shared/page-message-validation.ts);
//   мусор отбрасывается и считается;
// - портом участка 2 (его открывает фон через tabs.connect, когда начинает передачу): пока порта нет,
//   изолированный мир ничего фону не шлёт и фон не будит;
// - запросами «опиши видео документа» (для выбора видео фоном) с таймаутом;
// - страховкой тени: если порт фона пропал посреди передачи, MAIN-мир получает `end_handoff`
//   «остаться на позиции тени, играть как тень» — страница не остаётся навсегда в тени без плеера;
// - маршрутом событий: каждое событие страницы уходит порту СВОЕЙ передачи (по номеру запроса
//   `begin_handoff`), события старой передачи в сеанс новой не попадают.
// Сам ничего не решает о сеансе: нумерация поколений, очередь и плеер — в фоне.

import type {
  DescribeCandidatesQuery,
  PageCommand,
  PageEvent,
  VideoCandidate,
} from "../shared/handoff-contract.ts";
import { parseCandidatesReply, parsePageEvent } from "../shared/page-message-validation.ts";

/**
 * Сколько ждём ответ MAIN-мира на `describe_candidates`. MAIN-мир отвечает синхронно из своего
 * обработчика, задержка — только занятость потока страницы. 1 с с запасом покрывает тяжёлую
 * страницу; фрейм без нашего MAIN-скрипта (не успел встать) честно даёт пустой список.
 */
export const DESCRIBE_CANDIDATES_TIMEOUT_MS = 1_000;

/** Наибольший номер запроса (u32 по договору); после него счёт идёт с 1. */
const MAX_REQUEST_ID = 0xffff_ffff;

/** Таймеры изолированного мира (в продукте — setTimeout окна, в тестах — фейковое время). */
export interface RelayTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

/** Порт участка 2 (runtime.Port или фейк в тестах). */
export interface BackgroundPort {
  postMessage(message: unknown): void;
  disconnect(): void;
  readonly onMessage: { addListener(listener: (message: unknown) => void): void };
  readonly onDisconnect: { addListener(listener: () => void): void };
}

/** Счётчики пограничника — для тестов и диагностики (журнала в продукте нет). */
export interface RelayCounters {
  /** Сообщения MAIN-мира не договорной формы. */
  readonly rejectedPageMessages: number;
  /** Договорные события, которые некому отдать (нет передачи или её порт пропал). */
  readonly undeliverablePageEvents: number;
  /** Команды фона неизвестного вида. */
  readonly rejectedBackgroundMessages: number;
}

export interface ContentRelay {
  /** Сообщение из MAIN-мира (уже объект этого мира, но не проверенное). */
  handlePageMessage(message: unknown): void;
  /** Фон открыл порт участка 2 к этому фрейму. */
  acceptBackgroundPort(port: BackgroundPort): void;
  /** Описание видео документа от MAIN-мира; таймаут или пустой фрейм — пустой список. */
  describeVideoCandidates(): Promise<readonly VideoCandidate[]>;
  counters(): RelayCounters;
}

export interface RelayDependencies {
  /** Отправка MAIN-миру по личному порту участка 1. */
  readonly sendToPage: (message: PageCommand | DescribeCandidatesQuery) => void;
  readonly timers: RelayTimers;
  /** Одна строка диагностики за всю жизнь документа (первый мусор): без спама. */
  readonly debugOnce: (message: string) => void;
}

/**
 * Передача, чей поток сейчас идёт из MAIN-мира (после `handoff_began{started}` и до `handoff_ended`).
 * MAIN-мир держит не больше одной тени, а его события упорядочены: всё между `handoff_began` и
 * `handoff_ended` относится к ОДНОЙ передаче. `port` — порт фона, который её начал; `undefined` —
 * хозяин пропал (порт закрыт или заменён): тень уже снимается, хвост событий никому не отдаётся.
 */
interface StreamOwner {
  readonly requestId: number;
  port: BackgroundPort | undefined;
}

/** Снять тень без плеера: остаться на позиции тени, играть/стоять как тень. */
const END_HANDOFF_WITHOUT_PLAYER: PageCommand = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "as_shadow" },
};

interface PendingQuery {
  readonly resolve: (candidates: readonly VideoCandidate[]) => void;
  readonly timer: unknown;
}

export function createContentRelay(dependencies: RelayDependencies): ContentRelay {
  const { sendToPage, timers } = dependencies;
  const pendingQueries = new Map<number, PendingQuery>();
  let backgroundPort: BackgroundPort | undefined;
  // Начатые фоном передачи, на которые MAIN-мир ещё не ответил: номер запроса → порт-хозяин
  // (`undefined` — хозяин пропал до ответа: если тень всё же встанет, её сразу снимаем).
  const pendingBegins = new Map<number, BackgroundPort | undefined>();
  let streamOwner: StreamOwner | undefined;
  let nextRequestId = 1;
  let rejectedPageMessages = 0;
  let undeliverablePageEvents = 0;
  let rejectedBackgroundMessages = 0;

  function rejectPageMessage(): void {
    rejectedPageMessages += 1;
    if (rejectedPageMessages === 1) dependencies.debugOnce("fastiplayer bridge: page channel message rejected");
  }

  // ---------------------------------------------------------------- MAIN → фон

  function handlePageMessage(message: unknown): void {
    const reply = parseCandidatesReply(message);
    if (reply !== undefined) {
      settleQuery(reply.request_id, reply.candidates);
      return;
    }
    const event = parsePageEvent(message);
    if (event === undefined) {
      rejectPageMessage();
      return;
    }
    routePageEvent(event);
  }

  /**
   * Событие страницы уходит ТОЛЬКО порту той передачи, к которой оно относится. Так хвост старой
   * передачи (между нашим `end_handoff` и её `handoff_ended`) не попадает в сеанс новой, а фаза
   * новой передачи не сбивается событиями старой.
   */
  function routePageEvent(event: PageEvent): void {
    switch (event.kind) {
      case "handoff_began": {
        if (!pendingBegins.has(event.request_id)) {
          // Ответ на запрос, которого фон не делал (подделка страницы или давно забытый): некому.
          undeliverablePageEvents += 1;
          return;
        }
        const owner = pendingBegins.get(event.request_id);
        pendingBegins.delete(event.request_id);
        const ownerIsAlive = owner !== undefined && owner === backgroundPort;
        if (event.outcome === "started") {
          streamOwner = { requestId: event.request_id, port: ownerIsAlive ? owner : undefined };
          // Хозяин пропал, пока MAIN-мир начинал: тень без плеера сразу снимаем.
          if (!ownerIsAlive) sendToPage(END_HANDOFF_WITHOUT_PLAYER);
        }
        deliver(ownerIsAlive ? owner : undefined, event);
        return;
      }
      case "handoff_ended": {
        const owner = streamOwner?.port;
        streamOwner = undefined;
        deliver(owner, event);
        return;
      }
      default:
        deliver(streamOwner?.port, event);
    }
  }

  function deliver(port: BackgroundPort | undefined, event: PageEvent): void {
    if (port === undefined || port !== backgroundPort) {
      undeliverablePageEvents += 1;
      return;
    }
    try {
      port.postMessage(event.kind === "bytes" ? { ...event, bytes: exactBytes(event.bytes) } : event);
    } catch {
      // Порт уже мёртв: его onDisconnect придёт следом и снимет тень; событие отдать некому.
      undeliverablePageEvents += 1;
    }
  }

  // ---------------------------------------------------------------- фон → MAIN

  function acceptBackgroundPort(port: BackgroundPort): void {
    // Новый порт заменяет старый: старый закрываем так же, как если бы он оборвался.
    if (backgroundPort !== undefined) {
      const previous = backgroundPort;
      handleBackgroundGone(previous);
      safeDisconnect(previous);
    }
    backgroundPort = port;
    port.onMessage.addListener((message) => {
      if (backgroundPort === port) handleBackgroundCommand(port, message);
    });
    port.onDisconnect.addListener(() => handleBackgroundGone(port));
  }

  function handleBackgroundCommand(port: BackgroundPort, message: unknown): void {
    if (!isPageCommand(message)) {
      rejectedBackgroundMessages += 1;
      return;
    }
    if (message.kind === "begin_handoff") pendingBegins.set(message.request_id, port);
    sendToPage(message);
  }

  function handleBackgroundGone(port: BackgroundPort): void {
    if (backgroundPort !== port) return;
    backgroundPort = undefined;
    // Тень этого хозяина снимаем; её хвост до handoff_ended уже никому не отдаётся.
    if (streamOwner !== undefined && streamOwner.port === port) {
      streamOwner.port = undefined;
      sendToPage(END_HANDOFF_WITHOUT_PLAYER);
    }
    for (const [requestId, owner] of pendingBegins) if (owner === port) pendingBegins.set(requestId, undefined);
  }

  // ---------------------------------------------------------------- кандидаты видео

  function describeVideoCandidates(): Promise<readonly VideoCandidate[]> {
    const requestId = nextRequestId;
    nextRequestId = nextRequestId >= MAX_REQUEST_ID ? 1 : nextRequestId + 1;
    return new Promise((resolve) => {
      const timer = timers.setTimeout(() => settleQuery(requestId, []), DESCRIBE_CANDIDATES_TIMEOUT_MS);
      pendingQueries.set(requestId, { resolve, timer });
      sendToPage({ kind: "describe_candidates", request_id: requestId });
    });
  }

  function settleQuery(requestId: number, candidates: readonly VideoCandidate[]): void {
    const pending = pendingQueries.get(requestId);
    // Ответ на неизвестный/просроченный запрос — не мусор формы, просто опоздание: молча забываем.
    if (pending === undefined) return;
    pendingQueries.delete(requestId);
    timers.clearTimeout(pending.timer);
    pending.resolve(candidates);
  }

  return {
    handlePageMessage,
    acceptBackgroundPort,
    describeVideoCandidates,
    counters: () => ({ rejectedPageMessages, undeliverablePageEvents, rejectedBackgroundMessages }),
  };
}

/**
 * Байты ровно своего размера. Структурное клонирование `Uint8Array`-вида копирует ВЕСЬ его буфер
 * (разведка 09a: вид 1 МиБ на 16 МиБ буфере уходит в фон 16 МиБ). Если MAIN-мир прислал вид,
 * а не точную копию, режем здесь: копируются только нужные байты.
 */
export function exactBytes(bytes: Uint8Array): Uint8Array {
  const isExact = bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength;
  return isExact ? bytes : bytes.slice();
}

/** Команда фона известного вида (фон — свой, поэтому без глубокой проверки полей). */
function isPageCommand(message: unknown): message is PageCommand {
  if (typeof message !== "object" || message === null) return false;
  const kind = (message as { kind?: unknown }).kind;
  return kind === "begin_handoff" || kind === "shadow" || kind === "end_handoff";
}

function safeDisconnect(port: BackgroundPort): void {
  try {
    port.disconnect();
  } catch {
    // Порт уже закрыт браузером — освобождать нечего.
  }
}
