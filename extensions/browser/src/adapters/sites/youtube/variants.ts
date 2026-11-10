// Адаптер вариантов YouTube (изолированный мир): озвучки, качества, следующий ролик.
//
// YouTube игнорирует клики из кода (решение 207), поэтому ВСЁ, что нужно знать о плеере и переключить, идёт
// рукой-функцией (MAIN-мир, `player-hand.ts`). Рука DOM читает только кнопку «следующее видео».
//
// Описание строгое: ответ руки не той формы — исключение `YouTubePageError`, описание срывается целиком
// (полменю хуже, чем «меню нет»). Пункт, который нельзя показать (ключ вне алфавита протокола, повтор
// языка), молча пропускается: это не сбой формы, а данные сайта, которые мы не умеем выразить.

import { hostMatches } from "../../capabilities/ad-organs/ad-text.ts";
import type { AdFramePlace } from "../../capabilities/ad-organs/contract.ts";
import type {
  PageVariantDescription,
  PageVariantKind,
  PageVariantOption,
  PlayerHandResult,
  VariantActionOutcome,
  VariantAdapter,
  VariantHands,
} from "../../capabilities/variants/contract.ts";
import { resolutionStepLabel } from "../../capabilities/variants/quality-label.ts";
import type { HandValue } from "../../../shared/adapter-hand-contract.ts";
import { VARIANT_KEY_PATTERN, YOUTUBE_HAND_ACTIONS, YOUTUBE_VARIANT_MARKUP } from "./variant-markup.ts";

/** Рука-функция дала ответ не той формы: описание срывается целиком. */
export class YouTubePageError extends Error {}

/** Проверенное описание плеера, как его отдаёт `describe_player`. */
interface PlayerSnapshot {
  readonly videoId: string;
  readonly shorts: boolean;
  readonly qualities: readonly { readonly level: string; readonly label: string }[];
  readonly qualityPreference: string | null;
  readonly audio: readonly { readonly language: string; readonly label: string; readonly selected: boolean }[];
}

type Fields = Readonly<Record<string, HandValue>>;

function fieldsOf(candidate: HandValue, what: string): Fields {
  if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) throw new YouTubePageError(`${what} is not an object`);
  return candidate as Fields;
}

function listOf(candidate: HandValue | undefined, what: string): readonly HandValue[] {
  if (!Array.isArray(candidate)) throw new YouTubePageError(`${what} is not a list`);
  return candidate as readonly HandValue[];
}

/** Значение `done` или исключение: при описании полузнание хуже незнания. */
function doneValue(result: PlayerHandResult, action: string): HandValue {
  if (result.kind !== "done") throw new YouTubePageError(`${action}: ${result.kind}`);
  return result.value;
}

function snapshotFrom(value: HandValue): PlayerSnapshot {
  const fields = fieldsOf(value, "player");
  const { videoId, live, shorts, qualityPreference } = fields;
  if (typeof videoId !== "string" || videoId === "") throw new YouTubePageError("player videoId");
  if (typeof live !== "boolean" || typeof shorts !== "boolean") throw new YouTubePageError("player flags");
  if (qualityPreference !== null && typeof qualityPreference !== "string") throw new YouTubePageError("player qualityPreference");
  const qualities = listOf(fields.qualities, "qualities").map((entry) => {
    const { level, label } = fieldsOf(entry, "quality");
    if (typeof level !== "string" || typeof label !== "string") throw new YouTubePageError("quality fields");
    return { level, label };
  });
  const audio = listOf(fields.audio, "audio").map((entry) => {
    const { language, label, selected } = fieldsOf(entry, "audio track");
    if (typeof language !== "string" || typeof label !== "string" || typeof selected !== "boolean") throw new YouTubePageError("audio fields");
    return { language, label, selected };
  });
  // Эфир (`live`) отдельного поля в описании не требует: качества есть, озвучек нет — это уже в списках.
  return { videoId, shorts, qualities, qualityPreference, audio };
}

/** Исход действия руки-функции, которое отвечает `true` («сделано») или `false` («такого нет»). */
function actionOutcome(result: PlayerHandResult): VariantActionOutcome {
  if (result.kind !== "done") return "failed";
  if (result.value === true) return "done";
  return result.value === false ? "no_such_variant" : "failed";
}

/**
 * Меню качеств: «Auto» первым, дальше уровни YouTube; непригодные ключи и повторы пропускаем. Уровней нет
 * (встроенный плеер до старта) — меню нет: одинокое «Auto» зрителю ничего не даёт.
 */
function qualityMenu(snapshot: PlayerSnapshot): readonly PageVariantOption[] {
  const autoLevel = YOUTUBE_VARIANT_MARKUP.autoQualityLevel;
  const levels = snapshot.qualities.filter((quality) => quality.level !== autoLevel && VARIANT_KEY_PATTERN.test(quality.level));
  const unique = levels.filter((quality, index) => levels.findIndex((other) => other.level === quality.level) === index);
  if (unique.length === 0) return [];
  return [
    { id: autoLevel, label: "Auto", selected: snapshot.qualityPreference === autoLevel, automatic: true },
    // Подпись — ступень («720p», а не «720p60»): одинакова на всех роликах, память выбора её найдёт.
    ...unique.map((quality) => ({ id: quality.level, label: resolutionStepLabel(quality.label), selected: snapshot.qualityPreference === quality.level })),
  ];
}

/** Меню озвучек: id — код языка; повтор языка — остаётся первая дорожка. */
function audioMenu(snapshot: PlayerSnapshot): readonly PageVariantOption[] {
  const valid = snapshot.audio.filter((track) => VARIANT_KEY_PATTERN.test(track.language));
  return valid
    .filter((track, index) => valid.findIndex((other) => other.language === track.language) === index)
    .map((track) => ({ id: track.language, label: track.label, selected: track.selected }));
}

export function createYouTubeVariants(hands: VariantHands): VariantAdapter {
  const { dom, player } = hands;
  const markup = YOUTUBE_VARIANT_MARKUP;

  /** Есть ли следующий ролик: у кнопки непустой адрес и она не помечена выключенной (даже скрытая кнопка считается). */
  function nextButtonOffersVideo(): boolean {
    const button = dom.find(markup.nextButtonSelector);
    if (button === undefined) return false;
    const href = dom.attributeOf(button, markup.nextButtonHrefAttribute);
    return href !== null && href !== "" && dom.attributeOf(button, markup.nextButtonDisabledAttribute) !== "true";
  }

  return {
    matches: (place: AdFramePlace) => hostMatches(place.hostname, markup.hostSuffixes) && place.contains(markup.playerSelector),
    // Следим за всем документом, а не за первым `.html5-video-player`: в Shorts первым стоит ПУСТОЙ плеер, а
    // YouTube заменяет плееры при переходах. Рука DOM не видит функций плеера, активного отличить не может.
    observationRoot: () => dom.documentRoot(),
    async describe(): Promise<PageVariantDescription | null> {
      const value = doneValue(await player.call(YOUTUBE_HAND_ACTIONS.describePlayer, null), YOUTUBE_HAND_ACTIONS.describePlayer);
      if (value === null) return null; // активного плеера нет: ролик ещё не открыт
      const snapshot = snapshotFrom(value);
      return {
        seriesKey: markup.seriesKey,
        episodeKey: snapshot.videoId,
        audio: audioMenu(snapshot),
        quality: qualityMenu(snapshot),
        // Ленты Shorts — отдельная сессия (16b): «следующего» там не предлагаем.
        nextEpisode: !snapshot.shorts && nextButtonOffersVideo(),
      };
    },
    async select(kind: PageVariantKind, id: string): Promise<VariantActionOutcome> {
      // Ключ проверяем до обращения к странице: мусор ничего не вызывает.
      if (!VARIANT_KEY_PATTERN.test(id)) return "no_such_variant";
      const action = kind === "audio" ? YOUTUBE_HAND_ACTIONS.selectAudio : YOUTUBE_HAND_ACTIONS.selectQuality;
      return actionOutcome(await player.call(action, id));
    },
    async next(): Promise<VariantActionOutcome> {
      if (!nextButtonOffersVideo()) return "no_such_variant";
      return actionOutcome(await player.call(YOUTUBE_HAND_ACTIONS.nextVideo, null));
    },
  };
}
