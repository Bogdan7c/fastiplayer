// Нейтральный шов «подслушивания» передачи: контроллеры (главный поток и воркер) сообщают о
// ключевых шагах и об уходящих кусках байтов тому, кто подписался. В продуктовой сборке
// подписчика нет (`tap` не задан) и цена нулевая; отладочная сборка записи потока страницы
// (`stream-recorder/`) подставляет сюда своего подписчика.
//
// Правила шва:
//  - подписчик только наблюдает: исключение подписчика ловится и не ломает передачу;
//  - подписчик получает кусок непосредственно ДО отправки: отправка передаёт буфер (transfer) и
//    обнуляет его, так что после неё байтов уже не прочитать. Читать и копировать надо синхронно;
//  - байты куска — плотная копия отправителя; подписчик не должен их менять.

import type { LocalMediaSourceKey } from "../shared/handoff-contract.ts";
import type { IncomingPageMessage } from "./page-command-guard.ts";
import type { OddityReporter } from "./prototype-wrap.ts";

/** Откуда взялся кусок: повтор «памяти без копий» при начале передачи или живой поток. */
export type StreamOrigin = "replay" | "live";

/** Кусок байтов, ушедший из контроллера. */
export interface SentChunk {
  readonly mediaSource: LocalMediaSourceKey;
  readonly sourceBuffer: number;
  readonly bytes: Uint8Array;
  readonly origin: StreamOrigin;
}

/** Подписчик на уходящие куски (его хватает и воркеру). */
export interface ChunkTap {
  chunkSent(chunk: SentChunk): void;
}

/** Подписчик на шаги передачи в главном потоке. */
export interface HandoffTap extends ChunkTap {
  /** Передача началась (`handoff_began{started}` ушёл): элемент передачи. */
  handoffBegan(element: Element): void;
  /** Команда изолированного мира (кроме опроса кандидатов). */
  commandReceived(message: IncomingPageMessage): void;
  /** Повтор журнала главного потока отправлен целиком. */
  replayFinished(): void;
  /** Передача кончилась; `cause` — причина из договора (`returned`, `element_released`, …). */
  handoffEnded(cause: string): void;
}

/** Позвать подписчика так, чтобы его сбой не ушёл в передачу: след — одна запись о странности. */
export function notifyTap(reportOddity: OddityReporter, notify: () => void): void {
  try {
    notify();
  } catch {
    reportOddity("handoff tap failed");
  }
}
