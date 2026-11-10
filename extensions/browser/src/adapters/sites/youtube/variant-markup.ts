// Устройство плеера YouTube для адаптера вариантов — данные в одном месте, логика адаптера их не знает.
// Факты — из живой разведки 10.11.2026. Сайт сменил разметку или имена функций — правим только этот файл.
//
// Плеер YouTube — не глобальный объект окна, а DOM-элемент `.html5-video-player` с функциями на самом элементе.
// На странице ролика это `#movie_player`; в Shorts активный — `#shorts-player`, а пустой `#movie_player` тоже
// лежит в документе. Поэтому активный плеер ищем по содержимому, а не по id (см. `player-hand.ts`).

import { YOUTUBE_ORGANS } from "./ad-selectors.ts";

export const YOUTUBE_VARIANT_MARKUP = {
  /** Те же хосты, что у читателя рекламы: сам сайт и встраиваемый плеер. */
  hostSuffixes: YOUTUBE_ORGANS.hostSuffixes,
  /** Любой плеер YouTube (активный или пустой) — по нему же опознаём страницу в `matches`. */
  playerSelector: ".html5-video-player",
  /** Id активного плеера в Shorts. */
  shortsPlayerId: "shorts-player",
  /** Путь страницы Shorts (запасной признак, если id поменяют). */
  shortsPathPrefix: "/shorts",
  /** Кнопка «следующее видео»: есть ли следующий ролик, говорит её адрес, а не видимость. */
  nextButtonSelector: ".ytp-next-button",
  nextButtonHrefAttribute: "href",
  nextButtonDisabledAttribute: "aria-disabled",
  /** Постоянный ключ сериала: память выбора — одна запись на весь сайт (решение 289). */
  seriesKey: "youtube",
  /** Уровень «автоматика» YouTube: и пункт нашего меню, и аргумент `setPlaybackQualityRange`. */
  autoQualityLevel: "auto",
} as const;

/** Имена функций плеера YouTube (на элементе плеера). */
export const YOUTUBE_PLAYER_FUNCTIONS = {
  videoData: "getVideoData",
  availableQualities: "getAvailableQualityData",
  qualityPreference: "getUserPlaybackQualityPreference",
  setQualityRange: "setPlaybackQualityRange",
  availableAudioTracks: "getAvailableAudioTracks",
  currentAudioTrack: "getAudioTrack",
  setAudioTrack: "setAudioTrack",
  nextVideo: "nextVideo",
} as const;

/** Имена действий руки-функции этого адаптера: ключи белого списка в MAIN-мире. */
export const YOUTUBE_HAND_ACTIONS = {
  describePlayer: "describe_player",
  selectQuality: "select_quality",
  selectAudio: "select_audio",
  nextVideo: "next_video",
} as const;

/** Допустимый ключ пункта и аргумента действия (правило протокола: `A-Za-z0-9-_`, 1–64 символа). */
export const VARIANT_KEY_PATTERN = /^[A-Za-z0-9_-]{1,64}$/u;
