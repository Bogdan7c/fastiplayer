// Маршрутизация порта «bridge-status» на фейковом порте и фейковом поставщике статуса.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { routeStatusPort, type StatusPortLike } from "../src/background/status-port-router.ts";
import type { BridgeStatusProvider } from "../src/background/status-provider.ts";
import type { BridgeStatus } from "../src/shared/bridge-status.ts";
import type { StatusUpdateMessage } from "../src/shared/internal-messages.ts";

function createFakeProvider(initial: BridgeStatus) {
  let status = initial;
  const listeners = new Set<(status: BridgeStatus) => void>();
  const demand = { acquired: [] as string[], released: 0 };
  const provider: BridgeStatusProvider = {
    current: () => status,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    acquireDemand: (reason) => {
      demand.acquired.push(reason);
      return () => {
        demand.released += 1;
      };
    },
  };
  return {
    provider,
    demand,
    listeners,
    publish(next: BridgeStatus) {
      status = next;
      for (const listener of [...listeners]) listener(next);
    },
  };
}

function createFakePort(name: string) {
  const sent: StatusUpdateMessage[] = [];
  let disconnectListener: (() => void) | undefined;
  const port: StatusPortLike = {
    name,
    postMessage: (message) => void sent.push(message),
    onDisconnect: { addListener: (listener) => void (disconnectListener = listener) },
  };
  return { port, sent, disconnect: () => disconnectListener?.() };
}

describe("маршрутизатор порта bridge-status", () => {
  it("первое сообщение — текущий статус, обновления доходят, нужда занята как «popup»", () => {
    const fake = createFakeProvider({ state: "checking" });
    const popup = createFakePort("bridge-status");
    assert.equal(routeStatusPort(popup.port, fake.provider), true);
    assert.deepEqual(popup.sent, [{ kind: "status_update", status: { state: "checking" } }]);
    assert.deepEqual(fake.demand.acquired, ["popup"]);

    fake.publish({ state: "host_missing" });
    assert.deepEqual(popup.sent.at(-1), { kind: "status_update", status: { state: "host_missing" } });
    assert.equal(popup.sent.length, 2);
  });

  it("закрытие порта освобождает нужду ровно один раз и отписывает от обновлений", () => {
    const fake = createFakeProvider({ state: "unknown" });
    const popup = createFakePort("bridge-status");
    routeStatusPort(popup.port, fake.provider);
    popup.disconnect();
    popup.disconnect(); // повторный onDisconnect не должен освободить нужду второй раз
    assert.equal(fake.demand.released, 1);
    assert.equal(fake.listeners.size, 0);
    const sentBefore = popup.sent.length;
    fake.publish({ state: "host_missing" });
    assert.equal(popup.sent.length, sentBefore);
  });

  it("порт с другим именем игнорируется: нет нужды, подписки и сообщений", () => {
    const fake = createFakeProvider({ state: "unknown" });
    const stranger = createFakePort("something-else");
    assert.equal(routeStatusPort(stranger.port, fake.provider), false);
    assert.deepEqual(fake.demand.acquired, []);
    assert.equal(fake.listeners.size, 0);
    assert.deepEqual(stranger.sent, []);
  });

  it("два попапа держат нужду независимо", () => {
    const fake = createFakeProvider({ state: "unknown" });
    const first = createFakePort("bridge-status");
    const second = createFakePort("bridge-status");
    routeStatusPort(first.port, fake.provider);
    routeStatusPort(second.port, fake.provider);
    first.disconnect();
    assert.equal(fake.demand.released, 1);
    fake.publish({ state: "host_missing" });
    assert.equal(second.sent.at(-1)?.status.state, "host_missing");
    assert.equal(first.sent.length, 1);
  });

  it("сбой postMessage закрытого попапа не роняет фон", () => {
    const fake = createFakeProvider({ state: "unknown" });
    const popup = createFakePort("bridge-status");
    let calls = 0;
    const throwingPort: StatusPortLike = {
      ...popup.port,
      postMessage: () => {
        calls += 1;
        if (calls > 1) throw new Error("port closed");
      },
    };
    routeStatusPort(throwingPort, fake.provider);
    assert.doesNotThrow(() => fake.publish({ state: "host_missing" }));
    assert.equal(calls, 2);
  });
});
