// Обвязка тестов MAIN-мира: фейковая страница + настоящая сборка `startPageBridge` + канал
// участка 1 на настоящем MessageChannel Node (сообщения приходят асинхронно, как в браузере).

import { HANDSHAKE_MESSAGE } from "../../../src/shared/page-channel-handshake.ts";
import type { CandidatesReply, PageCommand, PageEvent, DescribeCandidatesQuery } from "../../../src/shared/handoff-contract.ts";
import { startPageBridge, type PageBridge } from "../../../src/page/bridge.ts";
import { captureShadowEnvironment, shadowStarterFor, type ShadowWindow } from "../../../src/page/shadow-assembly.ts";
import { createFakePage, FakeShadow, type FakePage } from "./fake-page.ts";
import type { WorkerHookSource } from "../../../src/page/worker-capture/index.ts";

export interface BridgeHarness {
  readonly page: FakePage;
  readonly bridge: PageBridge;
  readonly shadow: FakeShadow;
  readonly debugLines: string[];
}

export function startHarness(options: { memoryBudgetBytes?: number; page?: FakePage; workerHook?: WorkerHookSource } = {}): BridgeHarness {
  const page = options.page ?? createFakePage();
  const shadow = new FakeShadow();
  const debugLines: string[] = [];
  const bridge = startPageBridge(page.win, {
    startShadow: shadow.starter,
    debugSink: (line) => debugLines.push(line),
    ...(options.memoryBudgetBytes === undefined ? {} : { memoryBudgetBytes: options.memoryBudgetBytes }),
    ...(options.workerHook === undefined ? {} : { workerHook: options.workerHook }),
  });
  return { page, bridge, shadow, debugLines };
}

/** Сборка MAIN-мира с НАСТОЯЩЕЙ тенью, как в `page/main.ts`: окружение тени — первым. */
export interface RealShadowHarness {
  readonly page: FakePage;
  readonly bridge: PageBridge;
  readonly debugLines: string[];
}

export function startRealShadowHarness(): RealShadowHarness {
  const page = createFakePage();
  const debugLines: string[] = [];
  const shadowEnvironment = captureShadowEnvironment(page.win as unknown as ShadowWindow);
  if (shadowEnvironment === null) throw new Error("the fake page cannot host the real shadow");
  const bridge = startPageBridge(page.win, {
    startShadow: shadowStarterFor(shadowEnvironment),
    debugSink: (line) => debugLines.push(line),
  });
  return { page, bridge, debugLines };
}

/** Порт изолированного мира: шлёт команды и копит всё, что пришло из MAIN-мира. */
export class IsolatedSide {
  readonly received: (PageEvent | CandidatesReply)[] = [];
  readonly port: MessagePort;

  constructor(port: MessagePort) {
    this.port = port;
    port.addEventListener("message", (event) => {
      this.received.push((event as MessageEvent).data as PageEvent | CandidatesReply);
    });
    port.start();
  }

  send(message: PageCommand | DescribeCandidatesQuery | unknown): void {
    this.port.postMessage(message);
  }

  /** Дождаться, пока придёт не меньше `count` сообщений (или упасть по таймауту). */
  async waitFor(count: number, timeoutMs = 2000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (this.received.length < count) {
      if (Date.now() > deadline) throw new Error(`timeout: got ${this.received.length} of ${count} messages`);
      await new Promise((resolve) => setTimeout(resolve, 2));
    }
  }

  /** Дать сообщениям время дойти (для проверок «ничего не пришло»). */
  async settle(): Promise<void> {
    for (let round = 0; round < 5; round += 1) await new Promise((resolve) => setTimeout(resolve, 2));
  }

  close(): void {
    this.port.close();
  }
}

/** Изолированный мир делает рукопожатие: возвращает свою сторону канала. */
export function handshake(page: FakePage, message: unknown = HANDSHAKE_MESSAGE, source: unknown = page.win): IsolatedSide {
  const channel = new MessageChannel();
  page.win.dispatchEvent(new page.WindowMessageEvent("message", { data: message, ports: [channel.port2] }, source));
  return new IsolatedSide(channel.port1);
}
