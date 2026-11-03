// Номера потока одного сеанса: поколения и порядковые номера рамок (поправки 01 и 03b).
//
// Инварианты:
// - номер поколения получает каждый новый MediaSource; номера растут на 1 с 1 и НИКОГДА не
//   повторяются в сеансе (повтор `source_buffer_added` поколения 1 плеер закрывает 4003 — 03b);
// - MediaSource различаются ПО ДОКУМЕНТУ: ключ = «подключение порта участка 2» + локальный ключ
//   MAIN-мира («ms-1»). Перезагруженный документ снова начинает с «ms-1», но это новое подключение —
//   значит, новые номера;
// - порядковый номер рамки свой у каждой пары (поколение, SourceBuffer), с 0 и подряд: пропуск
//   приёмник считает потерей и закрывает сеанс.

import type { LocalMediaSourceKey } from "../../shared/handoff-contract.ts";
import type { SourceBufferIndex, StreamGeneration } from "../../protocol/index.ts";

/** Наибольший номер поколения (u32 протокола). */
const MAX_STREAM_GENERATION = 0xffff_ffff;

/** Номер поколения для локального ключа документа. */
export type GenerationLookup =
  | { readonly kind: "existing"; readonly generation: StreamGeneration }
  /** Ключ встречен впервые в этом документе: поколение только что открыто. */
  | { readonly kind: "started"; readonly generation: StreamGeneration }
  /** Номера u32 кончились (теоретический предел): сеанс продолжать нельзя. */
  | { readonly kind: "exhausted" };

/** Номера в пределах одного документа (одного подключения порта участка 2). */
export interface DocumentNumbering {
  /** Поколение MediaSource документа; первый взгляд на ключ открывает новое поколение. */
  generationOf(mediaSource: LocalMediaSourceKey): GenerationLookup;
  /**
   * Новое поколение-файл (`<video src>`, сессия 09b): у файла нет локального ключа — каждое
   * событие `file_source` открывает своё поколение по тем же правилам (растут на 1, не повторяются).
   */
  startFileGeneration(): Exclude<GenerationLookup, { kind: "existing" }>;
}

export interface StreamNumbering {
  /** Новый документ (новое подключение порта): свои ключи, общий счёт поколений. */
  openDocument(): DocumentNumbering;
  /** Последнее открытое поколение сеанса (`undefined` — ещё ни одного). */
  latestGeneration(): StreamGeneration | undefined;
  /** Следующий порядковый номер рамки SourceBuffer поколения (с 0n подряд). */
  nextFrameSequence(generation: StreamGeneration, sourceBuffer: SourceBufferIndex): bigint;
}

export function createStreamNumbering(): StreamNumbering {
  let lastGeneration = 0;
  const frameSequences = new Map<string, bigint>();

  function openGeneration(): StreamGeneration | undefined {
    if (lastGeneration >= MAX_STREAM_GENERATION) return undefined;
    lastGeneration += 1;
    return lastGeneration;
  }

  return {
    openDocument(): DocumentNumbering {
      // Ключи документа живут только в его записи: другой документ с тем же «ms-1» их не видит.
      const generationsByKey = new Map<LocalMediaSourceKey, StreamGeneration>();
      return {
        generationOf(mediaSource) {
          const known = generationsByKey.get(mediaSource);
          if (known !== undefined) return { kind: "existing", generation: known };
          const generation = openGeneration();
          if (generation === undefined) return { kind: "exhausted" };
          generationsByKey.set(mediaSource, generation);
          return { kind: "started", generation };
        },
        startFileGeneration() {
          const generation = openGeneration();
          return generation === undefined ? { kind: "exhausted" } : { kind: "started", generation };
        },
      };
    },
    latestGeneration: () => (lastGeneration === 0 ? undefined : lastGeneration),
    nextFrameSequence(generation, sourceBuffer) {
      const key = `${generation}:${sourceBuffer}`;
      const sequence = frameSequences.get(key) ?? 0n;
      frameSequences.set(key, sequence + 1n);
      return sequence;
    },
  };
}
