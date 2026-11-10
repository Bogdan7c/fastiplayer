// Фейковый мир для тестов передачи вкладки: время, WebSocket плеера, порт фрейма, «дверь» плеера.
import assert from "node:assert/strict";
import type { Scheduler, TimerHandle } from "../../src/background/host-link/native-port.ts";
import type { OpenSessionOutcome, OpenSessionRequest } from "../../src/background/host-link/open-session-requests.ts";
import type { FramePort } from "../../src/background/handoff/frame-port.ts";
import type { SessionDoor } from "../../src/background/handoff/handoff-session.ts";
import type { SessionWebSocket } from "../../src/background/handoff/session-socket.ts";
import type { PageCommand } from "../../src/shared/handoff-contract.ts";
import { CURRENT_PROTOCOL_VERSION, decodeDataFrame, decodePageSession, type GrantOutcome, type PageSessionMessage, type ProtocolVersion } from "../../src/protocol/index.ts";

// ------------------------------------------------------------------ время

export interface FakeClock {
  readonly scheduler: Scheduler;
  advance(ms: number): void;
  liveTimers(): number;
}

export function createFakeClock(): FakeClock {
  let nowMs = 0;
  let nextId = 1;
  const timers = new Map<number, { atMs: number; callback: () => void }>();
  return {
    scheduler: {
      setTimeout(callback: () => void, delayMs: number): TimerHandle {
        const id = nextId++;
        timers.set(id, { atMs: nowMs + delayMs, callback });
        return id;
      },
      clearTimeout(handle: TimerHandle): void {
        timers.delete(handle as number);
      },
      now: () => nowMs,
    },
    advance(ms) {
      const target = nowMs + ms;
      for (;;) {
        const due = [...timers.entries()].filter(([, timer]) => timer.atMs <= target).sort((a, b) => a[1].atMs - b[1].atMs || a[0] - b[0])[0];
        if (due === undefined) break;
        timers.delete(due[0]);
        nowMs = due[1].atMs;
        due[1].callback();
      }
      nowMs = target;
    },
    liveTimers: () => timers.size,
  };
}

// ------------------------------------------------------------------ WebSocket плеера

export const SOCKET_CONNECTING = 0;
export const SOCKET_OPEN = 1;
export const SOCKET_CLOSED = 3;

export class FakeWebSocket implements SessionWebSocket {
  binaryType: BinaryType = "blob";
  readyState = SOCKET_CONNECTING;
  bufferedAmount = 0;
  /** Всё, что ушло в сокет, в порядке отправки. */
  readonly sent: (string | Uint8Array)[] = [];
  /** Код, с которым экстеншен закрыл сокет (`undefined` — не закрывал). */
  closedWithCode: number | undefined;
  onopen: (() => void) | null = null;
  onmessage: ((event: { readonly data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: { readonly code: number }) => void) | null = null;

  readonly url: string;

  constructor(url: string) {
    this.url = url;
  }

  send(payload: string | Uint8Array<ArrayBuffer>): void {
    if (this.readyState !== SOCKET_OPEN) throw new Error("InvalidStateError: socket is not open");
    // Копия: вызывающий не должен менять уже отправленное.
    this.sent.push(typeof payload === "string" ? payload : new Uint8Array(payload));
  }

  close(code: number): void {
    this.closedWithCode = code;
    this.readyState = SOCKET_CLOSED;
  }

  /** Плеер принял соединение. */
  open(): void {
    this.readyState = SOCKET_OPEN;
    this.onopen?.();
  }

  /** Плеер прислал текстовое сообщение (объект сериализуется в JSON). */
  receive(message: Record<string, unknown> | string): void {
    this.onmessage?.({ data: typeof message === "string" ? message : JSON.stringify(message) });
  }

  receiveBinary(bytes: ArrayBuffer): void {
    this.onmessage?.({ data: bytes });
  }

  /** Плеер или сеть закрыли соединение. */
  remoteClose(code: number): void {
    this.readyState = SOCKET_CLOSED;
    this.onerror?.();
    this.onclose?.({ code });
  }

  /** Текстовые сообщения, разобранные тем же строгим кодеком, что у плеера. */
  sentMessages(): PageSessionMessage[] {
    return this.sent
      .filter((payload): payload is string => typeof payload === "string")
      .map((text) => {
        const decoded = decodePageSession(text);
        assert.ok(decoded.ok, `экстеншен послал невалидное сообщение: ${text}`);
        return decoded.value;
      });
  }

  /** Краткая запись провода: тип сообщения или «frame g/sb#seq». */
  wireSummary(): string[] {
    return this.sent.map((payload) => {
      if (typeof payload === "string") return (JSON.parse(payload) as { type: string }).type;
      const frame = decodeDataFrame(payload);
      assert.ok(frame.ok, "экстеншен послал невалидную рамку");
      const header = frame.value.header;
      if (header.kind === "source_buffer_bytes") return `frame ${header.generation}/${header.source_buffer}#${header.sequence}`;
      if (header.kind === "page_subtitle_bytes") return `subtitle ${header.track}@${header.offset}+${frame.value.payload.length}`;
      return `range ${header.request_id}@${header.offset}+${frame.value.payload.length}`;
    });
  }
}

export interface SocketFactoryRecorder {
  readonly sockets: FakeWebSocket[];
  readonly create: (url: string) => FakeWebSocket;
  last(): FakeWebSocket;
}

export function recordSockets(): SocketFactoryRecorder {
  const sockets: FakeWebSocket[] = [];
  return {
    sockets,
    create: (url) => {
      const socket = new FakeWebSocket(url);
      sockets.push(socket);
      return socket;
    },
    last: () => {
      const socket = sockets.at(-1);
      assert.ok(socket !== undefined, "сокет не открывался");
      return socket;
    },
  };
}

// ------------------------------------------------------------------ порт фрейма

export class FakeFramePort implements FramePort {
  readonly posted: PageCommand[] = [];
  disconnectedByBackground = false;
  private readonly messageListeners: ((message: unknown) => void)[] = [];
  private readonly disconnectListeners: (() => void)[] = [];
  readonly onMessage = { addListener: (listener: (message: unknown) => void) => void this.messageListeners.push(listener) };
  readonly onDisconnect = { addListener: (listener: () => void) => void this.disconnectListeners.push(listener) };

  readonly tabId: number;
  readonly frameId: number;

  constructor(tabId: number, frameId: number) {
    this.tabId = tabId;
    this.frameId = frameId;
  }

  /**
   * Порт умер молча (onDisconnect не пришёл), но только для таких команд: `postMessage` на них
   * бросает, как настоящий отсоединённый порт. По умолчанию — ни для каких.
   */
  failPostsWhere: (command: PageCommand) => boolean = () => false;

  postMessage(command: PageCommand): void {
    if (this.disconnectedByBackground) throw new Error("port is disconnected");
    if (this.failPostsWhere(command)) throw new Error("Attempting to use a disconnected port object");
    this.posted.push(command);
  }

  disconnect(): void {
    this.disconnectedByBackground = true;
  }

  /** Изолированный мир прислал событие страницы. */
  deliver(message: unknown): void {
    for (const listener of this.messageListeners) listener(message);
  }

  /** Страница ушла (документ выгружен или вкладка закрыта). */
  drop(): void {
    for (const listener of this.disconnectListeners) listener();
  }
}

export interface FrameConnectorRecorder {
  readonly ports: FakeFramePort[];
  readonly connect: (tabId: number, frameId: number) => FakeFramePort;
  last(): FakeFramePort;
  /** Какие команды будут бросать у портов, открытых ПОСЛЕ установки (порт мёртв с рождения). */
  failPostsOfNewPortsWhere: (command: PageCommand) => boolean;
}

export function recordFramePorts(): FrameConnectorRecorder {
  const ports: FakeFramePort[] = [];
  const recorder: FrameConnectorRecorder = {
    ports,
    failPostsOfNewPortsWhere: () => false,
    connect: (tabId, frameId) => {
      const port = new FakeFramePort(tabId, frameId);
      port.failPostsWhere = recorder.failPostsOfNewPortsWhere;
      ports.push(port);
      return port;
    },
    last: () => {
      const port = ports.at(-1);
      assert.ok(port !== undefined, "порт к фрейму не открывался");
      return port;
    },
  };
  return recorder;
}

// ------------------------------------------------------------------ дверь плеера (HostLink)

export const TEST_TOKEN = "T".repeat(32);
export const TEST_PORT = 47_001;

export interface FakeDoor extends SessionDoor {
  readonly requests: OpenSessionRequest[];
  demandHeld(): number;
  demandAcquired(): number;
  /** Следующий ответ на open_session (по умолчанию — пропуск). */
  nextOutcome: (request: OpenSessionRequest) => OpenSessionOutcome;
}

/** Выдача сеанса плеером; `outcome` — что плеер сделал с окном (по умолчанию открыл новое). */
export function grantFor(
  request: OpenSessionRequest,
  outcome: GrantOutcome = "started",
  agreedProtocol: ProtocolVersion = CURRENT_PROTOCOL_VERSION,
): OpenSessionOutcome {
  return {
    kind: "granted",
    agreedProtocol,
    grant: { type: "session_granted", request_id: 1, session_key: request.session_key, port: TEST_PORT, token: TEST_TOKEN, outcome },
  };
}

export function createFakeDoor(): FakeDoor {
  let held = 0;
  let acquired = 0;
  const door: FakeDoor = {
    requests: [],
    nextOutcome: (request) => grantFor(request),
    openSession(request) {
      door.requests.push(request);
      return Promise.resolve(door.nextOutcome(request));
    },
    acquireDemand() {
      held += 1;
      acquired += 1;
      let released = false;
      return () => {
        // Двойное освобождение — ошибка владельца сеанса: тест должен это увидеть.
        assert.equal(released, false, "нужда освобождена дважды");
        released = true;
        held -= 1;
      };
    },
    demandHeld: () => held,
    demandAcquired: () => acquired,
  };
  return door;
}

/**
 * Дать отработать уже разрешённым промисам (микрозадачам). 60 кругов: с сессии 15c опрос вкладки
 * спрашивает фреймы по одному (список фреймов, внедрение в каждый, общий срок), и путь от входа по
 * ссылке до сокета сеанса стал длиннее прежних 30 шагов.
 */
export async function flushPromises(): Promise<void> {
  for (let round = 0; round < 60; round += 1) await Promise.resolve();
}
