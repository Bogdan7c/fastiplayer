// Отправка одного элемента потока MSE (событие или кусок байтов) — общая для контроллера
// главного потока и для хука внутри воркера (сессия 09b).
//
// Правило байтов (решение 176, поправка 09a): журнал держит ВИДЫ на память страницы без копий;
// копия делается только в момент отправки — плотная (ровно кусок, а не весь буфер страницы) и
// передаётся дальше через transfer, то есть копия ровно одна. Кусок больше предела события
// (`MAX_PAGE_BYTES_EVENT_BYTES`, договор с изолированным миром) режется на части подряд.

import type { LocalMediaSourceKey, LocalMseEvent } from "../shared/handoff-contract.ts";
import { MAX_PAGE_BYTES_EVENT_BYTES } from "../shared/page-message-validation.ts";
import { copyForTransfer, type BinaryIntrinsics } from "./binary-view.ts";
import type { ReplayItem } from "./journal/media-journal.ts";

/** Сообщение потока в форме событий участка 1 (`mse` и `bytes`). */
export type StreamMessage =
  | { readonly kind: "mse"; readonly event: LocalMseEvent }
  | { readonly kind: "bytes"; readonly media_source: LocalMediaSourceKey; readonly source_buffer: number; readonly bytes: Uint8Array };

/** Куда уходит сообщение потока; `transfer` — буферы, которые передаются, а не копируются. */
export type StreamMessageSink = (message: StreamMessage, transfer: readonly Transferable[]) => void;

/**
 * Отправить элемент журнала или живого потока. Бросает, если копия не выделилась или отправка
 * отказала: решает вызывающий (у него и известно, чем кончать передачу).
 */
export function sendStreamItem(binary: BinaryIntrinsics, mediaSourceKey: LocalMediaSourceKey, item: ReplayItem, sink: StreamMessageSink): void {
  if (item.kind === "mse") {
    sink({ kind: "mse", event: item.event }, []);
    return;
  }
  for (let offset = 0; offset < item.view.length; offset += MAX_PAGE_BYTES_EVENT_BYTES) {
    const part = item.view.subarray(offset, offset + MAX_PAGE_BYTES_EVENT_BYTES);
    const bytes = copyForTransfer(binary, part);
    sink({ kind: "bytes", media_source: mediaSourceKey, source_buffer: item.sourceBuffer, bytes }, [bytes.buffer as ArrayBuffer]);
  }
}
