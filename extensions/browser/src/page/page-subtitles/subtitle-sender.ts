// Отправка событий субтитров в порт участка 1: точная копия байт с transfer, счётчик копий, один путь.
//
// Правило байтов то же, что у потока MSE (`stream-sender.ts`, решение 176): склад держит свои байты,
// в порт уходит ОДНА плотная копия (`copyForTransfer`) в списке transfer — склад остаётся целым, а
// структурное клонирование не тащит лишнего. Перед копией документ проходит тот же `ForwardByteMeter`,
// что и поток: страница не должна заставить нас копировать память без предела. В отличие от потока,
// отказ счётчика НЕ обрывает передачу: документ субтитров необязателен, и видео от его потери не страдает.
// Отказ — исход `refused`, страница о нём не узнаёт, одна отладочная запись.

import type { PageSubtitleDocumentEvent, PageSubtitleEvent, PageSubtitleTrackSourceEvent } from "../../shared/page-subtitle-contract.ts";
import { copyForTransfer, type BinaryIntrinsics } from "../binary-view.ts";
import type { ForwardByteMeter } from "../forward-byte-meter.ts";
import type { OddityReporter } from "../prototype-wrap.ts";

/** Что нужно отправке. */
export interface SubtitleSenderTools {
  readonly binary: BinaryIntrinsics;
  readonly meter: ForwardByteMeter;
  readonly reportOddity: OddityReporter;
}

/** Куда уходит событие; `transfer` — буферы, которые передаются, а не копируются. Может бросить. */
export type SubtitleEventSink = (event: PageSubtitleEvent, transfer: readonly Transferable[]) => void;

/** Поля события без `kind`: его ставит отправитель. */
export type SubtitleDocumentFields = Omit<PageSubtitleDocumentEvent, "kind">;
export type SubtitleTrackSourceFields = Omit<PageSubtitleTrackSourceEvent, "kind">;

/** Исход отправки документа: ушёл, счётчик копий отказал или порт отказал. */
export type SubtitleSendOutcome = "sent" | "refused" | "failed";

/** Отправитель событий субтитров одной передачи. */
export interface SubtitleSender {
  sendDocument(fields: SubtitleDocumentFields): SubtitleSendOutcome;
  sendTrackSource(fields: SubtitleTrackSourceFields): SubtitleSendOutcome;
}

export function createSubtitleSender(tools: SubtitleSenderTools, sink: SubtitleEventSink): SubtitleSender {
  /** Отправить, не пуская исключение порта наружу: потеря субтитров не повод рвать передачу. */
  function deliver(event: PageSubtitleEvent, transfer: readonly Transferable[]): SubtitleSendOutcome {
    try {
      sink(event, transfer);
      return "sent";
    } catch {
      tools.reportOddity("a subtitle event could not be sent");
      return "failed";
    }
  }
  return {
    sendDocument(fields) {
      const verdict = tools.meter.admit(tools.binary.typedArrayByteLength(fields.bytes));
      if (verdict !== "admitted") {
        tools.reportOddity(`a subtitle document was refused: ${verdict}`);
        return "refused";
      }
      const bytes = copyForTransfer(tools.binary, fields.bytes);
      return deliver({ kind: "subtitle_document", ...fields, bytes }, [bytes.buffer as ArrayBuffer]);
    },
    sendTrackSource(fields) {
      return deliver({ kind: "subtitle_track_source", ...fields }, []);
    },
  };
}
