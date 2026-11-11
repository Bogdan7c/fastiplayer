// WebSocket сеанса фон → плеер (участок «сеанс», решение 82): `attach` и упорядоченная очередь.
//
// Инварианты:
// - первым уходит `attach`; пока не пришёл `attached`, больше ничего не уходит — всё ждёт в очереди;
// - порядок сообщений и рамок — ровно порядок постановки (порядок страницы), очередь FIFO;
// - `buffer_state{full}` придерживает рамки MSE (вид 1, байты SourceBuffer); из-за FIFO всё, что
//   стоит за такой рамкой, тоже ждёт до `accepting` (иначе сообщение о новом поколении обогнало бы
//   байты старого);
// - рамки вида 2 (байты файла и ресурсов HLS) «полон» не держит: их плеер запросил сам, а для
//   поколений-файлов и HLS он `buffer_state` не шлёт вовсе. Иначе «полон», оставшийся от прошлого
//   MSE-поколения (реклама), навсегда держал бы байты следующего файла или HLS (сессия 10a2);
// - выше SOCKET_HIGH_WATER_BYTES в буфере сокета новые элементы не отдаём (обратное давление);
// - потолок очереди MAX_SESSION_QUEUE_BYTES: переполнение = страница не слушает «полон», сеанс
//   честно закрывается (молча выбросить кусок нельзя — у плеера была бы дыра);
// - конец сеанса сообщается владельцу РОВНО один раз (`onEnded`) с причиной.

import {
  SESSION_CLOSE_CODES,
  closeReasonFromCode,
  decodePlayerSession,
  encodeDataFrame,
  encodePageSession,
  type PageReopenFailure,
  type PageSessionMessage,
  type PlayerSessionMessage,
  type SessionCloseReason,
  type TabGoneReason,
} from "../../protocol/index.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import { ATTACH_REPLY_TIMEOUT_MS, MAX_SESSION_QUEUE_BYTES, SOCKET_DRAIN_POLL_MS, SOCKET_HIGH_WATER_BYTES } from "./constants.ts";
import type { OutgoingItem } from "./page-event-translator.ts";

/** Состояние WebSocket «открыт» (WebSocket.OPEN). */
const WEB_SOCKET_OPEN = 1;

/**
 * Код «закрытие без кода» (RFC 6455, 1005): так помечаем потерю соединения, замеченную по отказу
 * `send`, когда настоящего кода закрытия ещё нет.
 */
const SOCKET_SEND_FAILED_CODE = 1005;

/** Нормальное закрытие WebSocket (RFC 6455): сеанс закончен по договорённости. */
export const NORMAL_CLOSE_CODE = 1000;

/**
 * Причины, с которыми экстеншен сам кончает живой сеанс, сперва дописав плееру очередь страницы
 * (сессия 09a2): у плеера окно уже открыто, и молчаливое 1000 погасило бы его без объяснения.
 * - `page_took_video_back` (4009, решение 179) — страница честно забрала видео, вкладка на месте;
 * - `protocol_violation` (4003, как решение 132) — сбой на стороне страницы или тень замолчала:
 *   плеер покажет «Stream error — video returned to the page»;
 * - `next_episode_not_started` (4011, сессия 16a, решение 307) — серия доиграла, а следующую страница не
 *   поставила в срок; только плееру 1.9+ (`closeReasonForUnstartedNextEpisode`), старому — 4009.
 */
export type QueueDeliveringCloseReason = Extract<SessionCloseReason, "page_took_video_back" | "protocol_violation" | "next_episode_not_started">;

/** Та часть WebSocket, которой пользуется сеанс (в тестах — фейк). */
export interface SessionWebSocket {
  binaryType: BinaryType;
  readonly readyState: number;
  readonly bufferedAmount: number;
  send(payload: string | Uint8Array<ArrayBuffer>): void;
  close(code: number): void;
  onopen: (() => void) | null;
  onmessage: ((event: { readonly data: unknown }) => void) | null;
  onerror: (() => void) | null;
  onclose: ((event: { readonly code: number }) => void) | null;
}

export type WebSocketFactory = (url: string) => SessionWebSocket;

/** Почему сеанс по сокету закончился. */
export type SocketEndCause =
  /** Плеер (или сеть) закрыл соединение; `reason` — причина протокола по коду, если код наш. */
  | { readonly kind: "player_closed"; readonly code: number; readonly reason: SessionCloseReason | undefined }
  /** Не дождались `attached`. */
  | { readonly kind: "attach_timeout" }
  /** Плеер прислал то, чего протокол не допускает (мусор, двоичное, не то до `attached`). */
  | { readonly kind: "player_protocol_violation"; readonly detail: string }
  /** Очередь переполнена: страница не слушает «полон». */
  | { readonly kind: "queue_overflow" }
  /** Наше сообщение не прошло строгую запись протокола (баг или непроверенные данные страницы). */
  | { readonly kind: "page_message_rejected"; readonly detail: string }
  /** Сеанс закрыл сам экстеншен (`close`/`sendTabGoneAndClose`/`closeAfterDeliveringQueue`). */
  | { readonly kind: "closed_by_extension" };

export interface SessionSocketCallbacks {
  onAttached(): void;
  /** Сообщения плеера после `attached`, кроме `buffer_state` (его очередь разбирает сама). */
  onPlayerMessage(message: PlayerSessionMessage): void;
  /** Конец сеанса — ровно один раз. */
  onEnded(cause: SocketEndCause): void;
}

export interface SessionSocketParameters {
  readonly port: number;
  readonly sessionKey: string;
  readonly token: string;
  readonly createWebSocket: WebSocketFactory;
  readonly scheduler: Scheduler;
  readonly callbacks: SessionSocketCallbacks;
}

export interface SessionSocket {
  /** Поставить сообщение или рамку в очередь (порядок страницы). */
  enqueue(item: OutgoingItem): void;
  /** Страница ушла: отдать что можно, `tab_gone{reason}` и закрыть кодом 4002. */
  sendTabGoneAndClose(reason: TabGoneReason): void;
  /** Страница ушла, связь держим (сирота, 1.4): отдать что можно, `page_left{reason}`, сокет открыт. */
  sendPageLeft(reason: TabGoneReason): void;
  /** Вернуть страницу не вышло (1.4): отдать что можно, `page_reopen_failed{reason}` и закрыть кодом 4002. */
  sendPageReopenFailedAndClose(reason: PageReopenFailure): void;
  /**
   * Страница на месте, но передача кончилась по её стороне: отдать что можно и закрыть кодом
   * причины, без `tab_gone` (иначе плеер решил бы, что вкладка ушла, и стал бы доигрывать).
   */
  closeAfterDeliveringQueue(reason: QueueDeliveringCloseReason): void;
  /** Закрыть без сообщения (код — причина протокола или нормальное закрытие). */
  close(code: number): void;
  /** Байт в своей очереди (ещё не отданы сокету) — для тестов и диагностики. */
  queuedBytes(): number;
}

interface QueuedEntry {
  readonly payload: string | Uint8Array<ArrayBuffer>;
  /** Рамка MSE (вид 1): её придерживает `buffer_state{full}`. Сообщения и рамки вида 2 — нет. */
  readonly heldWhileIntakeFull: boolean;
  readonly sizeBytes: number;
}

type SocketPhase = "connecting" | "awaiting_attached" | "attached" | "ended";

export function openSessionSocket(parameters: SessionSocketParameters): SessionSocket {
  const { callbacks } = parameters;
  const attachTimer = createTimerSlot(parameters.scheduler);
  const drainTimer = createTimerSlot(parameters.scheduler);
  const queue: QueuedEntry[] = [];
  let queuedBytes = 0;
  let phase: SocketPhase = "connecting";
  let intakeIsFull = false;

  const socket = parameters.createWebSocket(`ws://127.0.0.1:${parameters.port}/`);
  socket.binaryType = "arraybuffer";
  socket.onopen = handleOpen;
  socket.onmessage = (event) => handleIncoming(event.data);
  // Ошибку WebSocket всегда сопровождает close: причину разбираем там.
  socket.onerror = () => {};
  socket.onclose = (event) => finish({ kind: "player_closed", code: event.code, reason: closeReasonFromCode(event.code) });

  // ---------------------------------------------------------------- конец

  function finish(cause: SocketEndCause, closeCode?: number): void {
    if (phase === "ended") return;
    phase = "ended";
    attachTimer.cancel();
    drainTimer.cancel();
    queue.length = 0;
    queuedBytes = 0;
    socket.onopen = null;
    socket.onmessage = null;
    socket.onclose = null;
    if (closeCode !== undefined) closeQuietly(closeCode);
    callbacks.onEnded(cause);
  }

  function closeQuietly(code: number): void {
    try {
      socket.close(code);
    } catch {
      // Сокет уже закрыт или код отвергнут — соединение всё равно уходит, причина уже сообщена.
    }
  }

  const violation = (detail: string): void =>
    finish({ kind: "player_protocol_violation", detail }, SESSION_CLOSE_CODES.protocol_violation);

  // ---------------------------------------------------------------- вход

  function handleOpen(): void {
    const attach = encodePageSession({ type: "attach", session_key: parameters.sessionKey, token: parameters.token });
    if (!attach.ok) {
      finish({ kind: "page_message_rejected", detail: `attach: ${attach.error.kind}` }, SESSION_CLOSE_CODES.protocol_violation);
      return;
    }
    phase = "awaiting_attached";
    attachTimer.start(ATTACH_REPLY_TIMEOUT_MS, () => finish({ kind: "attach_timeout" }, NORMAL_CLOSE_CODE));
    sendNow(attach.value);
  }

  function handleIncoming(data: unknown): void {
    if (phase === "ended") return;
    if (typeof data !== "string") {
      violation("binary frame from player");
      return;
    }
    const decoded = decodePlayerSession(data);
    if (!decoded.ok) {
      violation(`player message: ${decoded.error.kind}`);
      return;
    }
    const message = decoded.value;
    if (phase !== "attached") {
      if (message.type !== "attached") {
        violation(`${message.type} before attached`);
        return;
      }
      attachTimer.cancel();
      phase = "attached";
      callbacks.onAttached();
      pump();
      return;
    }
    if (message.type === "attached") {
      violation("attached twice");
      return;
    }
    if (message.type === "buffer_state") {
      intakeIsFull = message.intake === "full";
      if (!intakeIsFull) pump();
      return;
    }
    callbacks.onPlayerMessage(message);
  }

  // ---------------------------------------------------------------- выход

  function sendNow(payload: string | Uint8Array<ArrayBuffer>): boolean {
    try {
      socket.send(payload);
      return true;
    } catch {
      // send бросает только на неоткрытом сокете: соединение уже потеряно. Его close пришёл бы
      // следом, но сеанс кончаем сразу, чтобы ничего не ушло мимо порядка.
      finish({ kind: "player_closed", code: SOCKET_SEND_FAILED_CODE, reason: undefined }, NORMAL_CLOSE_CODE);
      return false;
    }
  }

  /** Отдаёт очередь сокету, пока можно: после `attached`, без «полон» для рамок, ниже порога буфера. */
  function pump(): void {
    while (phase === "attached" && queue.length > 0 && socket.readyState === WEB_SOCKET_OPEN) {
      const head = queue[0]!;
      if (head.heldWhileIntakeFull && intakeIsFull) return; // отпустит buffer_state{accepting}
      if (socket.bufferedAmount > SOCKET_HIGH_WATER_BYTES) {
        if (!drainTimer.isActive()) drainTimer.start(SOCKET_DRAIN_POLL_MS, pump);
        return;
      }
      queue.shift();
      queuedBytes -= head.sizeBytes;
      if (!sendNow(head.payload)) return;
    }
  }

  function encodeItem(item: OutgoingItem): QueuedEntry | string {
    if (item.kind === "message") {
      const encoded = encodePageSession(item.message);
      return encoded.ok
        ? { payload: encoded.value, heldWhileIntakeFull: false, sizeBytes: encoded.value.length }
        : `${item.message.type}: ${encoded.error.kind}`;
    }
    const frame = encodeDataFrame(item.header, item.payload);
    return frame.ok
      ? {
          payload: frame.value as Uint8Array<ArrayBuffer>,
          heldWhileIntakeFull: item.header.kind === "source_buffer_bytes",
          sizeBytes: frame.value.byteLength,
        }
      : `frame: ${frame.error.kind}`;
  }

  function enqueue(item: OutgoingItem): void {
    if (phase === "ended") return;
    const entry = encodeItem(item);
    if (typeof entry === "string") {
      finish({ kind: "page_message_rejected", detail: entry }, SESSION_CLOSE_CODES.protocol_violation);
      return;
    }
    if (queuedBytes + entry.sizeBytes > MAX_SESSION_QUEUE_BYTES) {
      finish({ kind: "queue_overflow" }, SESSION_CLOSE_CODES.protocol_violation);
      return;
    }
    queue.push(entry);
    queuedBytes += entry.sizeBytes;
    pump();
  }

  /**
   * Общий конец «со стороны страницы»: дописать очередь (порядок страницы), затем необязательное
   * последнее сообщение и закрытие кодом. Всё, что страница успела прислать, плеер получает ДО
   * конца — иначе показанное им расходилось бы с тем, что страница уже отдала.
   */
  function deliverQueueThenClose(lastMessage: PageSessionMessage | undefined, closeCode: number): void {
    if (phase === "ended") return;
    if (phase === "attached" && socket.readyState === WEB_SOCKET_OPEN) {
      if (!flushOrDropQueue()) return;
      if (lastMessage !== undefined) {
        const encoded = encodePageSession(lastMessage);
        // Своё постоянное сообщение не проходит запись только при баге кодека: закрытие кодом
        // всё равно сообщает причину, поэтому сеанс не держим.
        if (encoded.ok) sendNow(encoded.value);
      }
    }
    finish({ kind: "closed_by_extension" }, closeCode);
  }

  /**
   * Очередь сейчас целиком: при «принимаю» — отдать сокету, при «полон» — выбросить. Выброшенные рамки
   * плееру не нужны, а дыры в номерах не будет: после конца потока (или ухода страницы) рамок этого
   * поколения больше не бывает. Порог буфера сокета не ждём: решение принято сейчас. `false` — отправка
   * не ушла, сеанс уже кончен.
   */
  function flushOrDropQueue(): boolean {
    drainTimer.cancel();
    const entries = queue.splice(0);
    queuedBytes = 0;
    if (intakeIsFull) return true;
    for (const entry of entries) if (!sendNow(entry.payload)) return false;
    return true;
  }

  /**
   * Страница ушла, а связь остаётся «сиротой» (протокол 1.4, сессия 11b2): отдать очередь, как при конце, и
   * послать `page_left{reason}`, НЕ закрывая сокет. После `page_left` любой байт для плеера — нарушение 4003,
   * поэтому придержанные «полон» рамки ушедшего поколения выбрасываются, а не ждут «принимаю».
   * Сокет не на связи — ничего: его конец придёт владельцу обычным `onEnded`.
   */
  function sendPageLeft(reason: TabGoneReason): void {
    if (phase !== "attached" || socket.readyState !== WEB_SOCKET_OPEN) return;
    if (!flushOrDropQueue()) return;
    const encoded = encodePageSession({ type: "page_left", reason });
    if (encoded.ok) sendNow(encoded.value);
    else finish({ kind: "page_message_rejected", detail: `page_left: ${encoded.error.kind}` }, SESSION_CLOSE_CODES.protocol_violation);
  }

  function sendTabGoneAndClose(reason: TabGoneReason): void {
    deliverQueueThenClose({ type: "tab_gone", reason }, SESSION_CLOSE_CODES.tab_gone);
  }

  /** Вернуть страницу не вышло (протокол 1.4): `page_reopen_failed{reason}`, затем закрытие кодом 4002. */
  function sendPageReopenFailedAndClose(reason: PageReopenFailure): void {
    deliverQueueThenClose({ type: "page_reopen_failed", reason }, SESSION_CLOSE_CODES.tab_gone);
  }

  function closeAfterDeliveringQueue(reason: QueueDeliveringCloseReason): void {
    deliverQueueThenClose(undefined, SESSION_CLOSE_CODES[reason]);
  }

  return {
    enqueue,
    sendTabGoneAndClose,
    sendPageLeft,
    sendPageReopenFailedAndClose,
    closeAfterDeliveringQueue,
    close: (code) => finish({ kind: "closed_by_extension" }, code),
    queuedBytes: () => queuedBytes,
  };
}
