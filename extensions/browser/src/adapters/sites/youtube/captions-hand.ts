// Субтитры YouTube в MAIN-мире (сессия 15a2, решения 292, 293): описать дорожки и языки перевода плеера
// и включить выбранный язык через его же функцию `setOption("captions", "track", …)`.
//
// Данные берём из ответа плеера (`getPlayerResponse().captions.playerCaptionsTracklistRenderer`): он есть и
// во время рекламы, в отличие от `getOption("captions", "tracklist")`. Выбор делает сам YouTube: качает
// документ `timedtext` со своим защитным токеном, а мост ловит его как любую страницу-скачивание (13b).
//
// Всё, что уходит наружу, плоское и короткое: у `HandValue` глубина 4, ≤2048 узлов и строки ≤1024
// (`shared/adapter-hand-contract.ts`), а один длинный ответ страницы иначе сорвал бы описание целиком.

import { PAGE_FUNCTION_MISSING } from "../../capabilities/player-hand/contract.ts";
import { optionFingerprintOf } from "../../capabilities/variants/option-fingerprint.ts";
import { call, listFrom, nonEmptyString, read } from "./page-reading.ts";
import { VARIANT_KEY_PATTERN, YOUTUBE_CAPTIONS_MARKUP, YOUTUBE_PLAYER_FUNCTIONS } from "./variant-markup.ts";

const markup = YOUTUBE_CAPTIONS_MARKUP;
const functions = YOUTUBE_PLAYER_FUNCTIONS;

/** Родная дорожка ролика, как её отдаёт рука. */
interface NativeTrack {
  /** Наш id пункта меню: номер в списке YouTube + отпечаток `vssId` (защита от гонки, см. `trackItemId`). */
  readonly id: string;
  readonly language: string;
  /** `asr` — автосубтитры; у ручных пусто. */
  readonly kind: string;
  readonly vssId: string;
  /** Название дорожки у YouTube («British», «Lyrics»); может быть пустым. */
  readonly name: string;
  readonly translatable: boolean;
}

/** Выбор, который сейчас держит плеер. `translation` — язык автоперевода или `null`, если выбрана родная. */
interface CurrentCaptions {
  readonly vssId: string;
  readonly translation: string | null;
}

/** Что рука знает о субтитрах активного плеера. */
interface CaptionsFacts {
  readonly tracks: readonly NativeTrack[];
  /** Коды языков автоперевода, годные для id пункта. */
  readonly translations: readonly string[];
  readonly current: CurrentCaptions | null;
}

/** Строка не длиннее предела руки (длинное — режем, а не роняем описание). */
function clipped(text: string): string {
  return text.length > markup.maxFieldLength ? text.slice(0, markup.maxFieldLength) : text;
}

/**
 * Id родной дорожки: `t<номер>_<отпечаток vssId>`. Номер ведёт к дорожке, отпечаток проверяет, что на этом
 * месте по-прежнему та же дорожка: между описанием и выбором YouTube мог пересобрать список (смена ролика,
 * реклама). Не совпало — выбор честно «такого нет», а не молчаливая подмена соседом.
 */
function trackItemId(index: number, vssId: string): string {
  return `${markup.nativeIdPrefix}${index}_${optionFingerprintOf(vssId)}`;
}

/** Id пункта перевода. */
function translationItemId(languageCode: string): string {
  return `${markup.translationIdPrefix}${languageCode}`;
}

/** Название дорожки: `name.simpleText` или склейка `name.runs[].text`; нет — пустая строка. */
function trackNameOf(rawTrack: unknown): string {
  const name = read(rawTrack, "name");
  const simple = read(name, "simpleText");
  if (typeof simple === "string") return clipped(simple);
  const runs = listFrom(read(name, "runs"));
  return clipped(runs.map((run) => read(run, "text")).filter((text): text is string => typeof text === "string").join(""));
}

/** Разбор одной дорожки ответа; негодная (нет языка или `vssId`) — `undefined`. */
function nativeTrackFrom(rawTrack: unknown, index: number): NativeTrack | undefined {
  const language = read(rawTrack, "languageCode");
  const vssId = read(rawTrack, "vssId");
  if (!nonEmptyString(language) || !nonEmptyString(vssId)) return undefined;
  const kind = read(rawTrack, "kind");
  return {
    id: trackItemId(index, vssId),
    language: clipped(language),
    kind: typeof kind === "string" ? clipped(kind) : "",
    vssId: clipped(vssId),
    name: trackNameOf(rawTrack),
    translatable: read(rawTrack, "isTranslatable") === true,
  };
}

/** Что выбрано сейчас: `getOption("captions","track")`. Пустой объект и сбой плеера — «выбора нет». */
function currentCaptionsOf(player: object): CurrentCaptions | null {
  let option: unknown;
  try {
    option = call(player, functions.getOption, markup.module, markup.trackOption);
  } catch {
    // Выключенные субтитры плеер иногда отдаёт исключением, а не пустым объектом (разведка 15a2): это «выбора нет».
    return null;
  }
  const vssId = read(option, "vss_id");
  if (!nonEmptyString(vssId)) return null;
  const translation = read(read(option, "translationLanguage"), "languageCode");
  return { vssId: clipped(vssId), translation: nonEmptyString(translation) ? clipped(translation) : null };
}

/**
 * Факты о субтитрах плеера. `PAGE_FUNCTION_MISSING` — у плеера нет `getPlayerResponse` (страница не та).
 * Ответа с субтитрами нет (ролик без CC, эфир) — честные пустые списки.
 */
function captionsFactsOf(player: object): CaptionsFacts | typeof PAGE_FUNCTION_MISSING {
  const response = call(player, functions.playerResponse);
  if (response === PAGE_FUNCTION_MISSING) return PAGE_FUNCTION_MISSING;
  const renderer = read(read(response, markup.responseCaptions), markup.tracklistRenderer);
  const tracks = listFrom(read(renderer, markup.captionTracks))
    .slice(0, markup.maxNativeTracks)
    .flatMap((rawTrack, index) => nativeTrackFrom(rawTrack, index) ?? []);
  const translations = listFrom(read(renderer, markup.translationLanguages))
    .map((entry) => read(entry, "languageCode"))
    .filter((code): code is string => nonEmptyString(code) && VARIANT_KEY_PATTERN.test(translationItemId(code)))
    .slice(0, markup.maxTranslations);
  return { tracks, translations, current: currentCaptionsOf(player) };
}

/** Описание субтитров для `describe_player`: плоские списки; у плеера без таких функций — пустые. */
export function describeCaptions(player: object): unknown {
  const facts = captionsFactsOf(player);
  if (facts === PAGE_FUNCTION_MISSING) return { tracks: [], translations: [], current: null };
  return {
    tracks: facts.tracks.map((track) => ({ ...track })),
    translations: [...facts.translations],
    current: facts.current === null ? null : { ...facts.current },
  };
}

/**
 * Дорожка-источник автоперевода (решение 293): выбранная сейчас на странице, если её можно переводить;
 * иначе первая ручная переводимая; иначе первая переводимая (автосубтитры).
 */
function translationSourceOf(facts: CaptionsFacts): NativeTrack | undefined {
  const translatable = facts.tracks.filter((track) => track.translatable);
  const selected = translatable.find((track) => track.vssId === facts.current?.vssId);
  return selected ?? translatable.find((track) => track.kind !== markup.autoGeneratedKind) ?? translatable[0];
}

/** Параметр `setOption`: родная дорожка (поле `kind` у ручных субтитров YouTube не ждёт). */
function trackOption(track: NativeTrack): Record<string, unknown> {
  return track.kind === "" ? { languageCode: track.language, vss_id: track.vssId } : { languageCode: track.language, vss_id: track.vssId, kind: track.kind };
}

/** Включить субтитры пункта `itemId` (родная дорожка или перевод). Исходы: `true`, `false` («такого нет»), `PAGE_FUNCTION_MISSING`. */
export function selectCaptions(player: object, itemId: string | null): unknown {
  // Ключ проверяем до обращения к странице: мусор ничего не вызывает.
  if (itemId === null || !VARIANT_KEY_PATTERN.test(itemId)) return false;
  const facts = captionsFactsOf(player);
  if (facts === PAGE_FUNCTION_MISSING) return PAGE_FUNCTION_MISSING;
  let option: Record<string, unknown> | undefined;
  if (itemId.startsWith(markup.translationIdPrefix)) {
    const source = translationSourceOf(facts);
    const target = facts.translations.find((code) => translationItemId(code) === itemId);
    if (source !== undefined && target !== undefined) option = { ...trackOption(source), translationLanguage: { languageCode: target } };
  } else {
    const track = facts.tracks.find((candidate) => candidate.id === itemId);
    if (track !== undefined) option = trackOption(track);
  }
  if (option === undefined) return false;
  const outcome = call(player, functions.setOption, markup.module, markup.trackOption, option);
  return outcome === PAGE_FUNCTION_MISSING ? outcome : true;
}
