// Владелец номеров дорожек субтитров страницы на сеанс и их бюджета (сессия 13b2, протокол 1.5).
//
// Договор с плеером (limits.ts, зеркало assembly.rs):
// - номер дорожки живёт весь сеанс и выдаётся по паре «документ страницы + ключ страницы»: тот же ключ
//   того же документа - тот же номер (замена документа), новый документ или ключ - новый номер. Документ
//   узнаётся по фрейму и метке документа (`DocumentTag`, случайная на каждый документ), а НЕ по порту:
//   второе видео того же документа приходит новым портом, но склад MAIN-мира у документа тот же, и его
//   ключи обязаны попасть в те же номера (иначе в меню плеера дубли). Перезагруженный документ получает
//   новую метку - и новые номера. Номера НИКОГДА не повторяются;
// - не больше 64 разных номеров за сеанс (65-я дорожка - отказ, первые 64 живы);
// - бюджет 32 МиБ - СУММА последних объявленных длин каждой дорожки: замена считает новую длину
//   вместо старой (так же считает плеер).
// Реестр ничего не шлёт: он только решает, можно ли и под каким номером. Решённая замена фиксируется
// сразу - отправку после допуска сеанс не отменяет.

import { MAX_PAGE_SUBTITLE_SESSION_BYTES, MAX_PAGE_SUBTITLE_TRACKS } from "../../../protocol/limits.ts";
import type { PageSubtitleTrackId } from "../../../protocol/session/page-subtitles.ts";
import type { PageSubtitleKey } from "../../../shared/page-subtitle-contract.ts";

/** Какой документ страницы прислал дорожку: фрейм вкладки и метка документа. */
export interface SubtitleDocumentIdentity {
  readonly frameId: number;
  readonly documentTag: string;
}

/** Итог допуска документа дорожки. */
export type TrackAdmission =
  /** Допущена; `replaced` - номер уже был, документ заменяет прежний. */
  | { readonly kind: "admitted"; readonly track: PageSubtitleTrackId; readonly replaced: boolean }
  /** Все 64 номера заняты, а эта пара ключ+документ новая. */
  | { readonly kind: "too_many_tracks" }
  /** Сумма длин с этим документом перешла бы 32 МиБ; прежнее состояние не тронуто. */
  | { readonly kind: "over_budget" };

export interface TrackRegistry {
  /** Допустить документ длиной `byteLength` для дорожки (документ страницы, ключ). */
  admit(request: { readonly document: SubtitleDocumentIdentity; readonly key: PageSubtitleKey; readonly byteLength: number }): TrackAdmission;
  /** Сумма последних длин всех дорожек, байт (для журнала и тестов). */
  usedBytes(): number;
}

export function createTrackRegistry(): TrackRegistry {
  let lastTrack: PageSubtitleTrackId = 0;
  // Номер дорожки по паре «документ:ключ» и последняя объявленная длина каждой дорожки.
  const trackByPair = new Map<string, PageSubtitleTrackId>();
  const lengthByTrack = new Map<PageSubtitleTrackId, number>();
  let usedBytes = 0;

  return {
    admit({ document, key, byteLength }) {
      // Метка документа - только base64url (пограничник), поэтому «/» и «:» однозначно разделяют части.
      const pair = `${document.frameId}/${document.documentTag}:${key}`;
      const knownTrack = trackByPair.get(pair);
      const previousLength = knownTrack === undefined ? 0 : (lengthByTrack.get(knownTrack) ?? 0);
      // Замена считает новую длину вместо старой.
      if (usedBytes - previousLength + byteLength > MAX_PAGE_SUBTITLE_SESSION_BYTES) return { kind: "over_budget" };
      if (knownTrack !== undefined) {
        lengthByTrack.set(knownTrack, byteLength);
        usedBytes += byteLength - previousLength;
        return { kind: "admitted", track: knownTrack, replaced: true };
      }
      if (lastTrack >= MAX_PAGE_SUBTITLE_TRACKS) return { kind: "too_many_tracks" };
      lastTrack += 1;
      trackByPair.set(pair, lastTrack);
      lengthByTrack.set(lastTrack, byteLength);
      usedBytes += byteLength;
      return { kind: "admitted", track: lastTrack, replaced: false };
    },
    usedBytes: () => usedBytes,
  };
}
