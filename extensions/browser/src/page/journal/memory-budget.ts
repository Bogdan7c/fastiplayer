// Потолок памяти «памяти без копий» на один документ (решение 176).
//
// Все журналы MediaSource документа делят один потолок: страница с рекламой и сменой качества
// создаёт несколько MediaSource, и считать надо то, что держит документ целиком.
//
// Кто отдаёт память первым («старое — первым», 16b2): самый СТАРЫЙ MediaSource документа, а внутри
// него — самая старая запись. Следующий по возрасту MediaSource трогается, только когда у старшего
// записей не осталось (init не выкидывается никогда).
//
// Почему возраст MediaSource, а не возраст записи среди всех журналов. Лента коротких видео (Instagram,
// живая проверка 18 ноября 2026) при открытии страницы сразу качает НАЧАЛО трёх следующих роликов, по
// кругу, и только потом докачивает текущий. Самые старые записи документа — как раз эти начала. Когда
// документ доходил до потолка, прежнее правило выкидывало их первыми: при прокрутке повтор следующего
// ролика начинался с 5–15-й секунды, а страница начало второй раз не присылает (оно лежит у неё в
// SourceBuffer). Окно плеера ждало данных у нуля 10 с и вставало посреди ролика, а ролик, выкинутый
// целиком, обрывал сеанс. MediaSource же страница заводит в порядке ленты: пройденные ролики — самые
// старые, предстоящие — самые новые. Возраст MediaSource — момент, когда страница дала ему первый
// SourceBuffer (тогда появляется его журнал). На странице с одним живым MediaSource (фильм, эфир)
// правило совпадает с прежним.

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
  /** Журналы живых MediaSource в порядке их создания (на этот порядок опирается `enforce`). */
  readonly #journals = new Map<MediaJournal, JournalOwnerProbe>();

  constructor(capBytes: number = DOCUMENT_MEMORY_BUDGET_BYTES) {
    this.#capBytes = capBytes;
  }

  /**
   * Новый журнал для MediaSource. Заодно забываем журналы ушедших MediaSource (их байты и init
   * держали бы память страницы зря).
   */
  createJournal(key: LocalMediaSourceKey, owner: JournalOwnerProbe): MediaJournal {
    this.#forgetGoneJournals();
    const journal = new MediaJournal(key, { memory: this.memory });
    this.#journals.set(journal, owner);
    return journal;
  }

  /**
   * Привести память под потолок: самый старый MediaSource отдаёт свою самую старую запись, пока не
   * влезем (почему так — в шапке файла).
   */
  enforce(): void {
    while (this.memory.totalBytes > this.#capBytes) {
      const eldest = this.#eldestJournalWithEntries();
      // Записей не осталось: держат только init-сегменты (их не выкидываем — без них нет повтора).
      if (eldest === null) return;
      eldest.evictOldest();
    }
  }

  /**
   * Самый старый MediaSource документа, у которого ещё есть что выкинуть. `Map` перечисляет ключи в
   * порядке вставки, а журнал вставляется, когда его MediaSource получает первый SourceBuffer: порядок
   * перечисления и есть возраст MediaSource.
   */
  #eldestJournalWithEntries(): MediaJournal | null {
    for (const journal of this.#journals.keys()) {
      if (journal.hasEntriesToEvict()) return journal;
    }
    return null;
  }

  #forgetGoneJournals(): void {
    for (const [journal, owner] of this.#journals) {
      if (!owner.isGone()) continue;
      journal.discard();
      this.#journals.delete(journal);
    }
  }
}
