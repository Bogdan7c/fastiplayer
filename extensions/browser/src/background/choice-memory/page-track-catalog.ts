// Каталог дорожек субтитров страницы, предложенных плееру за сеанс (сессия 14b2), — для памяти выбора.
//
// Плеер сообщает выбор зрителя номером дорожки (`subtitle_choice{page_track N}`), а номера живут только
// в сеансе. Память же должна узнать дорожку и в следующей серии, поэтому помнит язык и подпись: каталог
// переводит номер в «язык + подпись» и обратно ищет дорожку по запомненному.
//
// Номера дорожек сеанса не сбрасываются, и дорожки прошлой серии остаются в каталоге до конца сеанса. Искать
// нужно только среди дорожек текущей серии, иначе вторая серия получила бы текст первой. Поэтому у каждой
// дорожки — время, когда её предложили, а поиск берёт только предложенные с начала серии.

import type { RememberedSubtitles } from "../../shared/choice-memory-contract.ts";

/** Дорожка, как её увидел плеер: номер сеанса, язык и подпись (`null` — неизвестны). */
export interface OfferedPageTrack {
  readonly track: number;
  readonly language: string | null;
  readonly label: string | null;
}

/** Запомненная дорожка (не «выключены»). */
export type RememberedTrack = Extract<RememberedSubtitles, { readonly kind: "track" }>;

export interface PageTrackCatalog {
  /** Дорожку предложили плееру (или переслали заново тем же номером) в момент `at`. */
  offered(track: OfferedPageTrack, at: number): void;
  /** Язык и подпись дорожки по номеру; нет такой — `undefined`. */
  describe(track: number): OfferedPageTrack | undefined;
  /**
   * Дорожка, предложенная не раньше `since`, для запомненного выбора: сначала совпадение языка и подписи,
   * потом — того же известного языка. Из равных — самая свежая. Нет — `undefined`.
   */
  find(wanted: RememberedTrack, since: number): number | undefined;
}

/** Предел каталога — как у реестра дорожек сеанса (`MAX_PAGE_SUBTITLE_TRACKS` = 64), с запасом на замены. */
const MAX_CATALOG_TRACKS = 256;

export function createPageTrackCatalog(): PageTrackCatalog {
  // Порядок вставки = порядок предложения: повтор того же номера переносится в конец.
  const tracks = new Map<number, { readonly track: OfferedPageTrack; readonly offeredAt: number }>();

  function newestSince(since: number, matches: (track: OfferedPageTrack) => boolean): number | undefined {
    let found: number | undefined;
    for (const { track, offeredAt } of tracks.values()) {
      if (offeredAt >= since && matches(track)) found = track.track;
    }
    return found;
  }

  return {
    offered(track, at) {
      tracks.delete(track.track);
      tracks.set(track.track, { track, offeredAt: at });
      // Самая старая дорожка уходит первой: она давно не текущей серии.
      if (tracks.size > MAX_CATALOG_TRACKS) {
        const oldest = tracks.keys().next();
        if (oldest.done !== true) tracks.delete(oldest.value);
      }
    },
    describe: (track) => tracks.get(track)?.track,
    find(wanted, since) {
      const exact = newestSince(since, (track) => track.language === wanted.language && track.label === wanted.label);
      if (exact !== undefined || wanted.language === null) return exact;
      return newestSince(since, (track) => track.language === wanted.language);
    },
  };
}
