// Потолок памяти «памяти без копий» на один документ (решение 176).
//
// Все журналы MediaSource документа делят один потолок: страница с рекламой и сменой качества
// создаёт несколько MediaSource, и считать надо то, что держит документ целиком. Выкидывается
// самая старая запись среди всех журналов («старое — первым»).

import { MediaJournal } from "./media-journal.ts";
import { RetainedMemory } from "./retained-memory.ts";
import type { LocalMediaSourceKey } from "../../shared/handoff-contract.ts";

/**
 * Потолок памяти документа: 64 МиБ.
 *
 * Откуда число: посреди просмотра страница держит в буфере 30–120 с вперёд (YouTube — до ~2 мин).
 * 1080p VP9/AV1 — около 2,5–4 Мбит/с, звук — ~0,13 Мбит/с; 120 с × 4,1 Мбит/с ≈ 62 МБ. Значит,
 * 64 МиБ хватает, чтобы при «отдать» плеер получил всю уже скачанную полку обычного ролика. Это
 * ССЫЛКИ на память, которую страница и так выделила: пока она сама держит те же куски, лишней
 * памяти нет; потолок ограничивает только то, что мы не даём ей освободить.
 */
export const DOCUMENT_MEMORY_BUDGET_BYTES = 64 * 1024 * 1024;

/** Чем живёт журнал снаружи: отсоединён ли его MediaSource (тогда его можно забыть). */
export interface JournalOwnerProbe {
  /** `true` — MediaSource собран сборщиком мусора или отсоединён от элемента (`closed`). */
  readonly isGone: () => boolean;
}

export class DocumentMemoryBudget {
  readonly memory = new RetainedMemory();
  readonly #capBytes: number;
  readonly #journals = new Map<MediaJournal, JournalOwnerProbe>();
  #lastOrder = 0;

  constructor(capBytes: number = DOCUMENT_MEMORY_BUDGET_BYTES) {
    this.#capBytes = capBytes;
  }

  /**
   * Новый журнал для MediaSource. Заодно забываем журналы ушедших MediaSource (их байты и init
   * держали бы память страницы зря).
   */
  createJournal(key: LocalMediaSourceKey, owner: JournalOwnerProbe): MediaJournal {
    this.#forgetGoneJournals();
    const journal = new MediaJournal(key, { memory: this.memory, nextOrder: () => this.#nextOrder() });
    this.#journals.set(journal, owner);
    return journal;
  }

  /** Привести память под потолок: выкидывать самую старую запись, пока не влезем. */
  enforce(): void {
    while (this.memory.totalBytes > this.#capBytes) {
      const oldest = this.#journalWithOldestEntry();
      // Записей не осталось: держат только init-сегменты (их не выкидываем — без них нет повтора).
      if (oldest === null) return;
      oldest.evictOldest();
    }
  }

  #nextOrder(): number {
    this.#lastOrder += 1;
    return this.#lastOrder;
  }

  #journalWithOldestEntry(): MediaJournal | null {
    let oldest: MediaJournal | null = null;
    let oldestOrder = Number.POSITIVE_INFINITY;
    for (const journal of this.#journals.keys()) {
      const order = journal.oldestOrder();
      if (order !== null && order < oldestOrder) {
        oldestOrder = order;
        oldest = journal;
      }
    }
    return oldest;
  }

  #forgetGoneJournals(): void {
    for (const [journal, owner] of this.#journals) {
      if (!owner.isGone()) continue;
      journal.discard();
      this.#journals.delete(journal);
    }
  }
}
