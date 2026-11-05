// Отправка одного элемента потока MSE (событие или кусок байтов) — общая для контроллера
// главного потока и для хука внутри воркера (сессия 09b).
//
// Правило байтов (решение 176, поправка 09a): журнал держит ВИДЫ на память страницы без копий;
// копия делается только в момент отправки — плотная (ровно кусок, а не весь буфер страницы) и
// передаётся дальше через transfer, то есть копия ровно одна. Кусок больше предела события
// (`MAX_PAGE_BYTES_EVENT_BYTES`, договор с изолированным миром) режется на части подряд.
// Перед копией кусок проходит счётчик `ForwardByteMeter`: страница не должна заставить нас копировать
// память без предела раньше, чем потолок очереди в фоне что-либо заметит (решение 200).
//
// Страница — враг: `length` и `subarray` живут на %TypedArray%.prototype, и она их подменяет. NaN вместо
// длины раньше проходил мимо счётчика, а `0` молча терял кусок. Поэтому длина и нарезка идут только через
// снятые на старте оригиналы (`BinaryIntrinsics`), и это единственный вход байтов в сеанс.

import type { LocalMediaSourceKey, LocalMseEvent } from "../shared/handoff-contract.ts";
import { MAX_PAGE_BYTES_EVENT_BYTES } from "../shared/page-message-validation.ts";
import { copyForTransfer, type BinaryIntrinsics } from "./binary-view.ts";
import type { ForwardByteMeter, ForwardVerdict } from "./forward-byte-meter.ts";
import type { ReplayItem } from "./journal/media-journal.ts";

/** Сообщение потока в форме событий участка 1 (`mse` и `bytes`). */
export type StreamMessage =
  | { readonly kind: "mse"; readonly event: LocalMseEvent }
  | { readonly kind: "bytes"; readonly media_source: LocalMediaSourceKey; readonly source_buffer: number; readonly bytes: Uint8Array };

/** Куда уходит сообщение потока; `transfer` — буферы, которые передаются, а не копируются. */
export type StreamMessageSink = (message: StreamMessage, transfer: readonly Transferable[]) => void;

/** Что нужно отправке кроме самого элемента. */
export interface StreamSendTools {
  readonly binary: BinaryIntrinsics;
  readonly meter: ForwardByteMeter;
}

/**
 * Счётчик отказал в копии: страница льёт байты сверх предела. Отдельный вид ошибки, потому что это вина
 * страницы (причина `page_flood` в модели вкладки), а не сбой отправки у нас.
 */
export class StreamBytesRefused extends Error {
  readonly verdict: Exclude<ForwardVerdict, "admitted">;

  constructor(verdict: Exclude<ForwardVerdict, "admitted">) {
    super(`stream bytes refused: ${verdict}`);
    this.verdict = verdict;
  }
}

/**
 * Отправить элемент журнала или живого потока. Бросает, если копия не выделилась или отправка отказала,
 * и `StreamBytesRefused`, если отказал счётчик: решает вызывающий (у него и известно, чем кончать передачу).
 */
export function sendStreamItem({ binary, meter }: StreamSendTools, mediaSourceKey: LocalMediaSourceKey, item: ReplayItem, sink: StreamMessageSink): void {
  if (item.kind === "mse") {
    sink({ kind: "mse", event: item.event }, []);
    return;
  }
  const byteLength = binary.typedArrayByteLength(item.view);
  const verdict = meter.admit(byteLength);
  if (verdict !== "admitted") throw new StreamBytesRefused(verdict);
  for (let offset = 0; offset < byteLength; offset += MAX_PAGE_BYTES_EVENT_BYTES) {
    const part = binary.typedArraySubarray(item.view, offset, offset + MAX_PAGE_BYTES_EVENT_BYTES);
    const bytes = copyForTransfer(binary, part);
    sink({ kind: "bytes", media_source: mediaSourceKey, source_buffer: item.sourceBuffer, bytes }, [bytes.buffer as ArrayBuffer]);
  }
}
