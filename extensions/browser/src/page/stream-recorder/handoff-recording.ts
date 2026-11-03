// Подписчик на шаги передачи: превращает шаги моста в события записи (`handoff_*`, `page_command`,
// `sent`). Только наблюдает (см. `handoff-tap.ts`): состояния передачи не касается.

import type { HandoffTap, SentChunk } from "../handoff-tap.ts";
import type { IncomingPageMessage } from "../page-command-guard.ts";
import type { JsonValue } from "./format.ts";
import type { StreamRecorder } from "./recorder.ts";

/** Копия значения через JSON (`JSON` снят на старте, до страницы): запись не должна меняться после. */
export type JsonCloner = (value: unknown) => JsonValue;

export class HandoffRecording implements HandoffTap {
  readonly #recorder: StreamRecorder;
  readonly #cloneJson: JsonCloner;
  /** Повтор журнала считается с начала передачи: сколько кусков и байт ушло как повтор. */
  #replayedItems = 0;
  #replayedBytes = 0;

  constructor(recorder: StreamRecorder, cloneJson: JsonCloner) {
    this.#recorder = recorder;
    this.#cloneJson = cloneJson;
  }

  handoffBegan(element: Element): void {
    this.#replayedItems = 0;
    this.#replayedBytes = 0;
    this.#recorder.record("handoff_began", { el: this.#recorder.elementIdOf(element) });
  }

  commandReceived(message: IncomingPageMessage): void {
    this.#recorder.record("page_command", { command: this.#cloneJson(message) });
  }

  chunkSent(chunk: SentChunk): void {
    const recorded = this.#recorder.recordSent(chunk.mediaSource, chunk.sourceBuffer, chunk.bytes, chunk.origin);
    if (recorded && chunk.origin === "replay") {
      this.#replayedItems += 1;
      this.#replayedBytes += chunk.bytes.length;
    }
  }

  replayFinished(): void {
    this.#recorder.record("handoff_replayed", { items: this.#replayedItems, bytes: this.#replayedBytes });
  }

  handoffEnded(cause: string): void {
    this.#recorder.record("handoff_ended", { cause });
  }
}
