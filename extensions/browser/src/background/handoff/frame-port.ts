// Порт участка 2 (фон ⇄ изолированный мир выбранного фрейма) в той мере, в какой он нужен сеансу.
// В продукте — runtime.Port из tabs.connect, в тестах — фейк.

import type { PageCommand } from "../../shared/handoff-contract.ts";

export interface FramePort {
  /** Бросает исключение, если порт уже закрыт. */
  postMessage(command: PageCommand): void;
  disconnect(): void;
  readonly onMessage: { addListener(listener: (message: unknown) => void): void };
  readonly onDisconnect: { addListener(listener: () => void): void };
}

/** Открыть порт к изолированному миру фрейма (`tabs.connect(tabId, {frameId, name})`). */
export type FrameConnector = (tabId: number, frameId: number) => FramePort;
