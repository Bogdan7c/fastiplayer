// Приём скачанного документа: проверка содержимым, имя по адресу, склад, оповещение.
//
// Общий конец у обоих хуков (`fetch` и XHR): ответ прошёл фильтр по адресу и типу, тело прочитано —
// теперь надо убедиться, что внутри действительно субтитры (иначе на склад попал бы любой JSON сайта),
// и отбросить кусочки HLS (WebVTT с `X-TIMESTAMP-MAP`): это не дорожка, hls.js соберёт её в `TextTrack`,
// а оттуда документ придёт репликами.

import { nameSubtitleDocumentByAddress } from "../../adapters/subtitle-naming/index.ts";
import { sniffSubtitleDocument } from "../../shared/subtitle-document-sniff.ts";
import type { DownloadedSubtitleStore, StoredSubtitleDocument } from "./downloaded-store.ts";

/** Что стало с предложенным документом. */
export type IntakeOutcome = "stored" | "hls_segment" | "not_subtitles" | "store_full";

export class SubtitleDownloadIntake {
  readonly #store: DownloadedSubtitleStore;
  readonly #onStored: (document: StoredSubtitleDocument) => void;

  constructor(store: DownloadedSubtitleStore, onStored: (document: StoredSubtitleDocument) => void) {
    this.#store = store;
    this.#onStored = onStored;
  }

  /** Предложить скачанные байты (`address` — откуда). Возвращает исход; ничего не бросает наружу из своих проверок. */
  offer(address: string, bytes: Uint8Array): IntakeOutcome {
    const verdict = sniffSubtitleDocument(bytes);
    if (verdict.kind === "hls_segment") return "hls_segment";
    if (verdict.kind === "not_subtitles") return "not_subtitles";
    const stored = this.#store.put({ naming: nameSubtitleDocumentByAddress(address), formatHint: verdict.format, bytes });
    if (stored === null) return "store_full";
    this.#onStored(stored);
    return "stored";
  }
}
