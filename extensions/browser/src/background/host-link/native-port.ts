// Швы связи с посредником: порт native messaging и планировщик времени.
// Продакшен-реализации — на chrome.runtime.connectNative и setTimeout; в тестах подставляются фейки.
import { browserApi } from "../../shared/browser-api.ts";

// Обработчики событий порта. Передаются при подключении, чтобы не потерять ранние события.
export interface NativePortHandlers {
  // Ответ посредника: уже разобранный браузером JSON (строгий разбор делает вызывающий).
  onMessage(message: unknown): void;
  // Порт закрыт; текст ошибки браузера, если он есть (Chrome — runtime.lastError, Firefox — port.error).
  onDisconnect(errorText: string | undefined): void;
}

export interface NativePort {
  // Бросает исключение, если порт уже закрыт.
  postMessage(message: unknown): void;
  disconnect(): void;
}

export interface NativePortFactory {
  // Бросает исключение, если браузер отказал сразу (нет разрешения, недопустимое имя).
  connect(hostName: string, handlers: NativePortHandlers): NativePort;
}

export type TimerHandle = unknown;

export interface Scheduler {
  setTimeout(callback: () => void, delayMs: number): TimerHandle;
  clearTimeout(handle: TimerHandle): void;
  // Монотонные миллисекунды для окна «посредник не запустился».
  now(): number;
}

export function createChromeNativePortFactory(): NativePortFactory {
  return {
    connect(hostName, handlers) {
      const port = browserApi.runtime.connectNative(hostName);
      port.onMessage.addListener((message: unknown) => handlers.onMessage(message));
      port.onDisconnect.addListener((closedPort) => {
        // Firefox кладёт причину в port.error, Chrome — в runtime.lastError (его нужно прочитать
        // внутри обработчика, иначе Chrome пишет в консоль «Unchecked runtime.lastError»).
        const firefoxError = (closedPort as { error?: { message?: string } }).error?.message;
        const chromeError = browserApi.runtime.lastError?.message;
        handlers.onDisconnect(firefoxError ?? chromeError);
      });
      return {
        postMessage: (message) => port.postMessage(message),
        disconnect: () => port.disconnect(),
      };
    },
  };
}

export function createRealScheduler(): Scheduler {
  return {
    setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
    clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    now: () => Date.now(),
  };
}
