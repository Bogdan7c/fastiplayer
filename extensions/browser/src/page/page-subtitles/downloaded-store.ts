// Склад скачанных документов субтитров документа страницы (сессия 13b2, решение 271).
//
// Плеер сайта качает субтитры, когда зритель их включил, — задолго до передачи видео. Пока передачи нет,
// документы лежат здесь и ничего не уходит; после `handoff_began{started}` склад отдаётся целиком.
//
// Свой склад, а не `DocumentMemoryBudget`: тот считает журнал MSE (сотни МиБ) и выселяет по его правилам,
// а здесь потолки свои и порядок выселения свой — самые старые документы уходят первыми.
//
// Инварианты:
//  - в складе не больше `MAX_STORED_SUBTITLE_DOCUMENTS` документов и `MAX_STORED_SUBTITLE_BYTES` байт;
//  - одинаковое содержимое (длина + два хеша) — та же дорожка: тот же ключ, и документ становится
//    самым свежим (сайт скачал его снова — зритель включил его снова);
//  - «самый свежий» документ — тот, что скачан последним; он и есть `showing` (решение 271), остальные —
//    `not_showing`. Склад хранит порядок, но не отправляет: отправка — дело `PageSubtitles`.

import type { PageSubtitleDisplay, SubtitleFormatHint } from "../../protocol/session/page-subtitles.ts";
import type { PageSubtitleKey, PageSubtitleNaming } from "../../shared/page-subtitle-contract.ts";
import { fingerprintOf, isSameContent, type ContentFingerprint } from "./content-fingerprint.ts";
import type { SubtitleKeyAllocator } from "./subtitle-keys.ts";

/**
 * Потолок склада, байт. 16 МиБ = два самых больших документа (8 МиБ по протоколу). Настоящие
 * субтитры фильма — 50–300 КиБ, так что потолок держит десятки дорожек и при этом не даёт странице,
 * которая льёт «субтитры» без конца, раздуть память вкладки.
 */
export const MAX_STORED_SUBTITLE_BYTES = 16 * 1024 * 1024;

/**
 * Потолок числа документов. Фильм с языками на все вкусы — 20–30 дорожек; 32 их покрывает и вдвое
 * меньше протокольных 64 дорожек сеанса, так что склад сам сеанс до его предела не доведёт.
 */
export const MAX_STORED_SUBTITLE_DOCUMENTS = 32;

/** Документ на складе. */
export interface StoredSubtitleDocument {
  readonly key: PageSubtitleKey;
  readonly naming: PageSubtitleNaming;
  readonly formatHint: SubtitleFormatHint;
  /** Своя копия байт (склад не держит память страницы); отправка копирует её ещё раз под transfer. */
  readonly bytes: Uint8Array;
  readonly fingerprint: ContentFingerprint;
}

/** Что кладут на склад. */
export interface SubtitleDocumentCandidate {
  readonly naming: PageSubtitleNaming;
  readonly formatHint: SubtitleFormatHint;
  readonly bytes: Uint8Array;
}

export class DownloadedSubtitleStore {
  readonly #keys: SubtitleKeyAllocator;
  /** Порядок вставки Map — порядок свежести: первый — самый старый, последний — самый свежий. */
  readonly #documents = new Map<PageSubtitleKey, StoredSubtitleDocument>();
  #totalBytes = 0;

  constructor(keys: SubtitleKeyAllocator) {
    this.#keys = keys;
  }

  /**
   * Положить документ. Одинаковое содержимое — тот же ключ, документ становится самым свежим;
   * иначе новый ключ и, при необходимости, выселение самых старых. `null` — ключи кончились.
   */
  put(candidate: SubtitleDocumentCandidate): StoredSubtitleDocument | null {
    const fingerprint = fingerprintOf(candidate.bytes);
    const same = this.#findSameContent(fingerprint);
    const key = same?.key ?? this.#keys.next();
    if (key === null) return null;
    if (same !== undefined) this.#remove(same);
    const document: StoredSubtitleDocument = { key, naming: candidate.naming, formatHint: candidate.formatHint, bytes: candidate.bytes, fingerprint };
    this.#documents.set(key, document);
    this.#totalBytes += document.bytes.length;
    this.#evictOldestWhileOverLimits(key);
    return document;
  }

  /** Документы от старого к свежему. */
  snapshot(): readonly StoredSubtitleDocument[] {
    return [...this.#documents.values()];
  }

  /** Как показывать документ: самый свежий — `showing`, прочие — `not_showing` (решение 271). */
  displayOf(key: PageSubtitleKey): PageSubtitleDisplay {
    let latest: PageSubtitleKey | null = null;
    for (const stored of this.#documents.keys()) latest = stored;
    return latest === key ? "showing" : "not_showing";
  }

  get documentCount(): number {
    return this.#documents.size;
  }

  get totalBytes(): number {
    return this.#totalBytes;
  }

  #findSameContent(fingerprint: ContentFingerprint): StoredSubtitleDocument | undefined {
    for (const stored of this.#documents.values()) {
      if (isSameContent(stored.fingerprint, fingerprint)) return stored;
    }
    return undefined;
  }

  #remove(document: StoredSubtitleDocument): void {
    if (this.#documents.delete(document.key)) this.#totalBytes -= document.bytes.length;
  }

  /** Выселить самые старые, пока склад выше потолков; только что положенный документ (`keepKey`) не трогаем. */
  #evictOldestWhileOverLimits(keepKey: PageSubtitleKey): void {
    for (const oldest of this.#documents.values()) {
      const overLimits = this.#documents.size > MAX_STORED_SUBTITLE_DOCUMENTS || this.#totalBytes > MAX_STORED_SUBTITLE_BYTES;
      if (!overLimits || oldest.key === keepKey) return;
      this.#remove(oldest);
    }
  }
}
