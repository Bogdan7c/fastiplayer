// Фейковый мир для тестов связи с посредником: фейковое время и фейковый порт.
import assert from "node:assert/strict";
import { createHostLink, type HostLink, type HostLinkDeps } from "../../src/background/host-link/host-link.ts";
import type {
  NativePort,
  NativePortFactory,
  NativePortHandlers,
  Scheduler,
  TimerHandle,
} from "../../src/background/host-link/native-port.ts";
import type { OpenSessionRequest } from "../../src/background/host-link/open-session-requests.ts";
import type { BridgeStatus } from "../../src/shared/bridge-status.ts";
import { decodeExtensionControl, type ExtensionControlMessage } from "../../src/protocol/index.ts";

export const TEST_EXTENSION_VERSION = "9.9.9";
export const TEST_HOST_NAME = "io.github.bogdan7c.fastiplayer.bridge";

export interface FakePort {
  readonly index: number;
  readonly connectedAtMs: number;
  readonly sentMessages: unknown[];
  closedByLink: boolean;
  // Посредник/плеер прислал сообщение.
  reply(message: unknown): void;
  // Браузер закрыл порт (посредник упал / не найден).
  drop(errorText?: string): void;
  // Что именно ушло в порт, разобранное тем же кодеком протокола, что и на стороне посредника.
  sentControl(): ExtensionControlMessage[];
}

interface TimerEntry {
  readonly id: number;
  readonly atMs: number;
  readonly callback: () => void;
}

export interface FakeWorld {
  readonly link: HostLink;
  readonly ports: FakePort[];
  readonly statuses: BridgeStatus[];
  nowMs(): number;
  advance(ms: number): void;
  liveTimerCount(): number;
  // Если задано, connect() бросает исключение (браузер отказал сразу).
  failConnectWith: string | undefined;
  connectTimes(): number[];
}

export function createWorld(extraDeps: Partial<Pick<HostLinkDeps, "onPlayerCheapVideoCodecs">> = {}): FakeWorld {
  let nowMs = 0;
  let nextTimerId = 1;
  const timers = new Map<number, TimerEntry>();
  const ports: FakePort[] = [];
  const statuses: BridgeStatus[] = [];

  const scheduler: Scheduler = {
    setTimeout(callback: () => void, delayMs: number): TimerHandle {
      const id = nextTimerId++;
      timers.set(id, { id, atMs: nowMs + delayMs, callback });
      return id;
    },
    clearTimeout(handle: TimerHandle): void {
      timers.delete(handle as number);
    },
    now: () => nowMs,
  };

  const world: FakeWorld = {
    link: undefined as unknown as HostLink,
    ports,
    statuses,
    nowMs: () => nowMs,
    advance(ms: number): void {
      const targetMs = nowMs + ms;
      // Таймеры срабатывают строго по порядку времени; новые, поставленные по ходу, тоже учитываются.
      for (;;) {
        const due = [...timers.values()]
          .filter((entry) => entry.atMs <= targetMs)
          .sort((left, right) => left.atMs - right.atMs || left.id - right.id)[0];
        if (due === undefined) break;
        timers.delete(due.id);
        nowMs = due.atMs;
        due.callback();
      }
      nowMs = targetMs;
    },
    liveTimerCount: () => timers.size,
    failConnectWith: undefined,
    connectTimes: () => ports.map((port) => port.connectedAtMs),
  };

  const portFactory: NativePortFactory = {
    connect(hostName: string, handlers: NativePortHandlers): NativePort {
      assert.equal(hostName, TEST_HOST_NAME);
      if (world.failConnectWith !== undefined) throw new Error(world.failConnectWith);
      let dropped = false;
      const fake: FakePort = {
        index: ports.length,
        connectedAtMs: nowMs,
        sentMessages: [],
        closedByLink: false,
        reply: (message) => {
          if (!dropped && !fake.closedByLink) handlers.onMessage(message);
        },
        drop: (errorText) => {
          dropped = true;
          handlers.onDisconnect(errorText);
        },
        sentControl: () =>
          fake.sentMessages.map((message) => {
            const decoded = decodeExtensionControl(JSON.stringify(message));
            assert.ok(decoded.ok, "в порт ушло сообщение, которое не принимает разбор протокола");
            return decoded.value;
          }),
      };
      ports.push(fake);
      return {
        postMessage(message: unknown): void {
          if (dropped || fake.closedByLink) throw new Error("port closed");
          fake.sentMessages.push(message);
        },
        disconnect(): void {
          fake.closedByLink = true;
        },
      };
    },
  };

  const link = createHostLink({
    portFactory,
    scheduler,
    hostName: TEST_HOST_NAME,
    extensionVersion: TEST_EXTENSION_VERSION,
    warn: () => {},
    ...extraDeps,
  });
  (world as { link: HostLink }).link = link;
  link.subscribe((status) => statuses.push(status));
  return world;
}

// ---- готовые ответы посредника/плеера

export function helloAccepted(playerVersion = "0.9.0", agreedProtocol = { major: 1, minor: 0 }): unknown {
  return {
    type: "hello_accepted",
    agreed_protocol: agreedProtocol,
    player: { protocol: { major: 1, minor: 0 }, software_version: playerVersion, capabilities: [] },
  };
}

export function refusedHello(reasonKind: string, extra: Record<string, unknown> = {}): unknown {
  return { type: "refused", in_reply_to: { kind: "hello" }, reason: { kind: reasonKind, ...extra } };
}

export function refusedOpenSession(requestId: number, reasonKind: string): unknown {
  return { type: "refused", in_reply_to: { kind: "open_session", request_id: requestId }, reason: { kind: reasonKind } };
}

export function sessionGranted(requestId: number, sessionKey: string): unknown {
  return {
    type: "session_granted",
    request_id: requestId,
    session_key: sessionKey,
    port: 4000,
    token: "a".repeat(32),
    outcome: "started",
  };
}

export function sampleRequest(sessionKey = "tab-1-video-1"): OpenSessionRequest {
  return {
    session_key: sessionKey,
    page: { title: "Фильм", domain: "example.com" },
    intent: "window",
    hints: { duration: { kind: "finite", duration_us: 90_000_000 }, position: { kind: "unknown" } },
  };
}

export function lastPort(world: FakeWorld): FakePort {
  const port = world.ports[world.ports.length - 1];
  assert.ok(port !== undefined, "порт ещё не открывали");
  return port;
}
