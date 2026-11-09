// Выдача ключей дорожек внутри документа страницы (`PageSubtitleKey`, 1..2^32-1).
//
// Ключ один на дорожку: склад скачанных документов, `<track>` и `TextTrack` берут номера из одного
// счётчика, чтобы ключи не пересекались. Тот же ключ ещё раз — замена документа этой дорожки.

import type { PageSubtitleKey } from "../../shared/page-subtitle-contract.ts";
import { MAX_U32 } from "../../shared/page-value-checks.ts";

export class SubtitleKeyAllocator {
  #last = 0;

  /** Следующий свободный ключ; `null` — все 2^32-1 выданы (документ не живёт так долго, но молча не переполняемся). */
  next(): PageSubtitleKey | null {
    if (this.#last >= MAX_U32) return null;
    this.#last += 1;
    return this.#last;
  }
}
