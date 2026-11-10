// Адаптер вариантов YouTube (изолированный мир): озвучки, качества, следующий ролик.
//
// YouTube игнорирует клики из кода (решение 207), поэтому ВСЁ, что нужно знать о плеере и переключить, идёт
// рукой-функцией (MAIN-мир, `player-hand.ts`). Рука DOM читает только кнопку «следующее видео».
//
// Описание строгое: ответ руки не той формы — исключение `YouTubePageError`, описание срывается целиком
// (полменю хуже, чем «меню нет»). Единственное исключение — субтитры (15a2): они независимая добавка, и их
// неразборчивый ответ даёт пустое меню субтитров, а рабочие озвучки и качества не страдают. Пункт, который нельзя показать (ключ вне алфавита протокола, повтор
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
import { captionLabelOf, englishLanguageName } from "../../capabilities/subtitle-naming/caption-label.ts";
import { withResolutionStepLabels } from "../../capabilities/variants/quality-label.ts";
import type { HandValue } from "../../../shared/adapter-hand-contract.ts";
import { VARIANT_KEY_PATTERN, YOUTUBE_CAPTIONS_MARKUP, YOUTUBE_HAND_ACTIONS, YOUTUBE_VARIANT_MARKUP } from "./variant-markup.ts";

/** Рука-функция дала ответ не той формы: описание срывается целиком. */
export class YouTubePageError extends Error {}

/** Родная дорожка субтитров, как её отдаёт рука (`captions-hand.ts`). */
interface CaptionTrackSnapshot {
  readonly id: string;
  readonly language: string;
  readonly kind: string;
  readonly vssId: string;
  readonly name: string;
  readonly translatable: boolean;
}

/** Субтитры плеера: дорожки ролика, коды автоперевода и текущий выбор страницы. */
interface CaptionsSnapshot {
  readonly tracks: readonly CaptionTrackSnapshot[];
  readonly translations: readonly string[];
  readonly current: { readonly vssId: string; readonly translation: string | null } | null;
}

/** Проверенное описание плеера, как его отдаёт `describe_player`. */
interface PlayerSnapshot {
  readonly videoId: string;
  readonly shorts: boolean;
  readonly qualities: readonly { readonly level: string; readonly label: string }[];
  readonly qualityPreference: string | null;
  readonly audio: readonly { readonly language: string; readonly label: string; readonly selected: boolean }[];
  /** `undefined` — рука не прислала субтитры или прислала мусор: меню субтитров пусто, остальное живо. */
  readonly captions: CaptionsSnapshot | undefined;
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
  const captions = captionsSnapshotFrom(fields.captions);
  // Эфир (`live`) отдельного поля в описании не требует: качества есть, озвучек нет — это уже в списках.
  return { videoId, shorts, qualities, qualityPreference, audio, captions };
}

/** Разбор одной дорожки субтитров; не та форма — `YouTubePageError`. */
function captionTrackFrom(entry: HandValue): CaptionTrackSnapshot {
  const { id, language, kind, vssId, name, translatable } = fieldsOf(entry, "caption track");
  if (typeof id !== "string" || typeof language !== "string" || typeof kind !== "string") throw new YouTubePageError("caption track fields");
  if (typeof vssId !== "string" || typeof name !== "string" || typeof translatable !== "boolean") throw new YouTubePageError("caption track fields");
  return { id, language, kind, vssId, name, translatable };
}

/**
 * Строгий разбор субтитров. Любое несоответствие формы — `undefined` (меню субтитров пусто): субтитры
 * необязательны, и мусор в них не должен отнимать у зрителя рабочие меню озвучек и качеств.
 */
function captionsSnapshotFrom(value: HandValue | undefined): CaptionsSnapshot | undefined {
  if (value === undefined) return undefined;
  try {
    const fields = fieldsOf(value, "captions");
    const tracks = listOf(fields.tracks, "caption tracks").map(captionTrackFrom);
    const translations = listOf(fields.translations, "caption translations").map((code) => {
      if (typeof code !== "string") throw new YouTubePageError("caption translation code");
      return code;
    });
    const rawCurrent = fields.current;
    if (rawCurrent === undefined) throw new YouTubePageError("caption current");
    let current: CaptionsSnapshot["current"] = null;
    if (rawCurrent !== null) {
      const { vssId, translation } = fieldsOf(rawCurrent, "caption current");
      if (typeof vssId !== "string" || (translation !== null && typeof translation !== "string")) throw new YouTubePageError("caption current fields");
      current = { vssId, translation };
    }
    return { tracks, translations, current };
  } catch (error) {
    // Не та форма субтитров - ожидаемое дело чужой страницы; любая другая ошибка - наш баг, глушить нельзя.
    if (error instanceof YouTubePageError) return undefined;
    throw error;
  }
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
  // Подпись — ступень («720p», а не «720p60»): одинакова на всех роликах, память выбора её найдёт.
  return withResolutionStepLabels([
    { id: autoLevel, label: "Auto", selected: snapshot.qualityPreference === autoLevel, automatic: true },
    ...unique.map((quality) => ({ id: quality.level, label: quality.label, selected: snapshot.qualityPreference === quality.level })),
  ]);
}

/** Меню озвучек: id — код языка; повтор языка — остаётся первая дорожка. */
function audioMenu(snapshot: PlayerSnapshot): readonly PageVariantOption[] {
  const valid = snapshot.audio.filter((track) => VARIANT_KEY_PATTERN.test(track.language));
  return valid
    .filter((track, index) => valid.findIndex((other) => other.language === track.language) === index)
    .map((track) => ({ id: track.language, label: track.label, selected: track.selected }));
}

/** Подпись родной дорожки; нет английского названия языка — название YouTube, а нет и его — сам код. */
function nativeCaptionLabel(track: CaptionTrackSnapshot, withTrackName: boolean): string {
  const label = captionLabelOf({
    language: track.language,
    autoGenerated: track.kind === YOUTUBE_CAPTIONS_MARKUP.autoGeneratedKind,
    autoTranslated: false,
    ...(withTrackName && track.name !== "" ? { trackName: track.name } : {}),
  });
  if (label !== "") return label;
  return track.name !== "" ? track.name : track.language;
}

/**
 * Меню субтитров: сначала родные дорожки в порядке YouTube, потом автоперевод на языки сайта; всего не больше
 * `maxMenuOptions` (режутся переводы). Родные дорожки с одинаковой подписью (два «English») различаются
 * названием YouTube в скобках. Перевод предлагаем, только если есть что переводить, и только на языки, у
 * которых есть английское название: безымянный язык зритель не отличит, а память не найдёт по подписи.
 */
function subtitlesMenu(snapshot: PlayerSnapshot): readonly PageVariantOption[] {
  const captions = snapshot.captions;
  if (captions === undefined) return [];
  const { current } = captions;
  const usable = captions.tracks.filter((track) => VARIANT_KEY_PATTERN.test(track.id));
  const baseLabels = usable.map((track) => nativeCaptionLabel(track, false));
  const natives = usable.map((track, index): PageVariantOption => {
    const isAmbiguous = baseLabels.filter((label) => label === baseLabels[index]).length > 1;
    return {
      id: track.id,
      label: isAmbiguous ? nativeCaptionLabel(track, true) : (baseLabels[index] ?? ""),
      selected: current !== null && current.translation === null && current.vssId === track.vssId,
    };
  });
  const canTranslate = captions.tracks.some((track) => track.translatable);
  const translations = canTranslate
    ? captions.translations
        .filter((code) => englishLanguageName(code) !== undefined && VARIANT_KEY_PATTERN.test(`${YOUTUBE_CAPTIONS_MARKUP.translationIdPrefix}${code}`))
        .map((code): PageVariantOption => ({
          id: `${YOUTUBE_CAPTIONS_MARKUP.translationIdPrefix}${code}`,
          label: captionLabelOf({ language: code, autoGenerated: false, autoTranslated: true }),
          selected: current !== null && current.translation === code,
        }))
    : [];
  return [...natives, ...translations].slice(0, YOUTUBE_CAPTIONS_MARKUP.maxMenuOptions);
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
        subtitles: subtitlesMenu(snapshot),
        // Ленты Shorts — отдельная сессия (16b): «следующего» там не предлагаем.
        nextEpisode: !snapshot.shorts && nextButtonOffersVideo(),
      };
    },
    async select(kind: PageVariantKind, id: string): Promise<VariantActionOutcome> {
      // Ключ проверяем до обращения к странице: мусор ничего не вызывает.
      if (!VARIANT_KEY_PATTERN.test(id)) return "no_such_variant";
      const action = {
        audio: YOUTUBE_HAND_ACTIONS.selectAudio,
        quality: YOUTUBE_HAND_ACTIONS.selectQuality,
        subtitles: YOUTUBE_HAND_ACTIONS.selectCaptions,
      }[kind];
      return actionOutcome(await player.call(action, id));
    },
    async next(): Promise<VariantActionOutcome> {
      if (!nextButtonOffersVideo()) return "no_such_variant";
      return actionOutcome(await player.call(YOUTUBE_HAND_ACTIONS.nextVideo, null));
    },
  };
}
