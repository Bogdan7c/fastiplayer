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
//   `begin_handoff`), события старой передачи в сеанс новой не попадают;
// - бюджетом событий передачи (page-event-budget.ts, решение 200): при превышении пересылка
//   останавливается сразу, а фон узнаёт об этом своим донесением `page_flood`, которое страница
//   подделать не может: оно рождается здесь, а не приходит из MAIN-мира;
// - тем, что документ говорит о себе сам: имя хоста и происхождение читаются здесь, в изолированном
//   мире, а не берутся на веру из MAIN-мира. От них зависят проверка адреса (решение 199) и Referer
//   (решение 202), поэтому поддельное значение страницы до фона не доходит.
// Сам ничего не решает о сеансе: нумерация поколений, очередь и плеер — в фоне.

import {
  PAGE_FLOOD_NOTICE_KIND,
  type DescribeCandidatesQuery,
  type PageCommand,
  type PageEvent,
  type PageEventBudgetKind,
  type PageFloodNotice,
  type VideoCandidate,
} from "../shared/handoff-contract.ts";
import { parseCandidatesReply, parsePageEvent } from "../shared/page-message-validation.ts";
import type { MillisecondClock } from "../shared/token-bucket.ts";
import { createPageEventBudget, type PageEventBudget, type PageEventBudgetParameters } from "./page-event-budget.ts";

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
  /** События, отброшенные после превышения бюджета: до фона они не дошли. */
  readonly droppedFloodEvents: number;
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
  /** Часы для бюджета событий передачи. */
  readonly now: MillisecondClock;
  /** Что документ знает о себе сам, прочитанное в ЭТОМ мире (страница подменить не может). */
  readonly ownDocument: () => OwnDocumentAddress;
  /** Свои лимиты бюджета (тесты); по умолчанию боевые. */
  readonly budgetLimits?: Omit<PageEventBudgetParameters, "now">;
}

/** Адрес документа глазами изолированного мира. */
export interface OwnDocumentAddress {
  /** `location.hostname`; "" — нет. */
  readonly hostname: string;
  /** `location.origin`; `null` — непрозрачное происхождение. */
  readonly origin: string | null;
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
  /** Бюджет именно этой передачи: следующая начинает с полного. */
  readonly budget: PageEventBudget;
  /** Бюджет кончился: пересылка остановлена, проходит только `handoff_ended`. */
  flooded: boolean;
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
  let droppedFloodEvents = 0;

  function rejectPageMessage(): void {
    rejectedPageMessages += 1;
    if (rejectedPageMessages === 1) dependencies.debugOnce("fastiplayer bridge: page channel message rejected");
    chargeMalformedMessage();
  }

  // ---------------------------------------------------------------- бюджет передачи

  /** Передача, чей поток ещё можно пересылать; бюджет считается только для неё. */
  function liveStreamOwner(): StreamOwner | undefined {
    return streamOwner?.port !== undefined && streamOwner.port === backgroundPort ? streamOwner : undefined;
  }

  /** Мусор во время передачи тратит её бюджет: тысячи кривых сообщений в секунду — это тоже флуд. */
  function chargeMalformedMessage(): void {
    const owner = liveStreamOwner();
    if (owner === undefined || owner.flooded) return;
    if (owner.budget.admit("malformed") === "exceeded") reportFlood(owner, "malformed");
  }

  /**
   * Бюджет кончился: сперва пересылка стоит (`flooded`), потом донесение фону. Конец сеанса и снятие
   * тени решает фон. Если донесение не ушло (порт мёртв), тень снимет `onDisconnect` порта; если
   * фон завис, он сам заметит тишину пульса: `shadow_alive` тоже больше не проходит.
   */
  function reportFlood(owner: StreamOwner, overBudget: PageEventBudgetKind): void {
    owner.flooded = true;
    const notice: PageFloodNotice = { kind: PAGE_FLOOD_NOTICE_KIND, over_budget: overBudget };
    try {
      owner.port?.postMessage(notice);
    } catch {
      undeliverablePageEvents += 1;
    }
  }

  /** Вид бюджета; `undefined` — событие бюджет не считает (`bytes`: у них потолок очереди). */
  function budgetKindOf(event: PageEvent): PageEventBudgetKind | undefined {
    switch (event.kind) {
      case "mse":
      case "page_seek":
      case "player_seek_applied":
      case "page_playback":
      case "file_source":
      case "shadow_alive":
        return event.kind;
      default:
        return undefined;
    }
  }

  /** Событие потока: сначала бюджет, потом доставка. После флуда не проходит ничего, и байты тоже. */
  function forwardStreamEvent(event: PageEvent): void {
    const owner = liveStreamOwner();
    if (owner === undefined) {
      deliver(undefined, event);
      return;
    }
    if (owner.flooded) {
      droppedFloodEvents += 1;
      return;
    }
    const budgetKind = budgetKindOf(event);
    if (budgetKind !== undefined && owner.budget.admit(budgetKind) === "exceeded") {
      droppedFloodEvents += 1;
      reportFlood(owner, budgetKind);
      return;
    }
    deliver(owner.port, withOwnDocumentFacts(event));
  }

  /** Заменить то, что страница сказала о своём документе, на прочитанное в этом мире. */
  function withOwnDocumentFacts(event: PageEvent): PageEvent {
    if (event.kind !== "file_source" || event.referrer_origin === null) return event;
    return { ...event, referrer_origin: dependencies.ownDocument().origin };
  }

  // ---------------------------------------------------------------- MAIN → фон

  function handlePageMessage(message: unknown): void {
    const reply = parseCandidatesReply(message);
    if (reply !== undefined) {
      // Ответ на незаданный вопрос — опоздание, но страница может сыпать ими так же, как мусором.
      if (!settleQuery(reply.request_id, reply.candidates)) chargeMalformedMessage();
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
          streamOwner = {
            requestId: event.request_id,
            port: ownerIsAlive ? owner : undefined,
            budget: createPageEventBudget({ now: dependencies.now, ...dependencies.budgetLimits }),
            flooded: false,
          };
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
        forwardStreamEvent(event);
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
      const timer = timers.setTimeout(() => void settleQuery(requestId, []), DESCRIBE_CANDIDATES_TIMEOUT_MS);
      pendingQueries.set(requestId, { resolve, timer });
      sendToPage({ kind: "describe_candidates", request_id: requestId });
    });
  }

  /** Кандидат с именем хоста, прочитанным здесь: от него зависит проверка адреса файла (решение 199). */
  function withOwnHostName(candidate: VideoCandidate): VideoCandidate {
    return { ...candidate, frame_host: dependencies.ownDocument().hostname };
  }

  /** Отдать ответ ждущему; `false` — ответ на незаданный или просроченный запрос (не мусор формы). */
  function settleQuery(requestId: number, candidates: readonly VideoCandidate[]): boolean {
    const pending = pendingQueries.get(requestId);
    if (pending === undefined) return false;
    pendingQueries.delete(requestId);
    timers.clearTimeout(pending.timer);
    pending.resolve(candidates.map(withOwnHostName));
    return true;
  }

  return {
    handlePageMessage,
    acceptBackgroundPort,
    describeVideoCandidates,
    counters: () => ({ rejectedPageMessages, undeliverablePageEvents, rejectedBackgroundMessages, droppedFloodEvents }),
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
