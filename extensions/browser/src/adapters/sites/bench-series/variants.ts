// Адаптер вариантов страницы «сериал» стенда (изолированный мир).
//
// - Озвучки — из разметки: кнопки `[data-dub]`, выбранная помечена `aria-pressed`. Выбор — клик (рука DOM).
// - Качества и «есть ли следующая серия» — из функций плеера страницы (рука-функция). Выбор качества и
//   следующая серия — тоже вызовом функций.
// Любой сбой руки-функции при описании — исключение: наблюдатель фрейма поймает его, и меню у плеера не будет
// (стенд так проверяет «адаптер упал → обычный перехват»).

import type { AdElement, AdFramePlace } from "../../capabilities/ad-organs/contract.ts";
import type {
  PageVariantDescription,
  PageVariantKind,
  PageVariantOption,
  PlayerHandResult,
  VariantActionOutcome,
  VariantAdapter,
  VariantHands,
} from "../../capabilities/variants/contract.ts";
import type { HandValue } from "../../../shared/adapter-hand-contract.ts";
import { BENCH_SERIES_HAND_ACTIONS, BENCH_SERIES_MARKUP } from "./markup.ts";

/** Рука-функция не дала того, что адаптер ждёт: описание срывается целиком. */
export class BenchSeriesPageError extends Error {}

/** Значение `done` или исключение: при описании полузнание хуже незнания. */
function doneValue(result: PlayerHandResult, action: string): HandValue {
  if (result.kind !== "done") throw new BenchSeriesPageError(`${action}: ${result.kind}`);
  return result.value;
}

/** Пункт качества из ответа страницы `{id, label, selected}` и необязательного `automatic` («Auto» сайта, 14b2). */
function qualityOption(candidate: HandValue): PageVariantOption {
  if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) throw new BenchSeriesPageError("quality is not an object");
  const { id, label, selected, automatic } = candidate as Readonly<Record<string, HandValue>>;
  if (typeof id !== "string" || typeof label !== "string" || typeof selected !== "boolean") throw new BenchSeriesPageError("quality fields");
  if (automatic === undefined) return { id, label, selected };
  if (typeof automatic !== "boolean") throw new BenchSeriesPageError("quality automatic");
  return { id, label, selected, automatic };
}

/** Исход действия руки-функции, которое отвечает `true` («сделано») или `false` («такого нет»). */
function actionOutcome(result: PlayerHandResult): VariantActionOutcome {
  if (result.kind !== "done") return "failed";
  if (result.value === true) return "done";
  return result.value === false ? "no_such_variant" : "failed";
}

export function createBenchSeriesVariants(hands: VariantHands): VariantAdapter {
  const { dom, player } = hands;
  const markup = BENCH_SERIES_MARKUP;

  const root = (): AdElement | undefined => dom.find(markup.rootSelector);
  const dubButtons = (within: AdElement): readonly AdElement[] => dom.findAll(markup.dubButtonSelector, within);

  function dubOption(button: AdElement): PageVariantOption | undefined {
    const id = dom.attributeOf(button, markup.dubKeyAttribute);
    if (id === null) return undefined;
    return { id, label: dom.textOf(button), selected: dom.attributeOf(button, markup.pressedAttribute) === "true" };
  }

  async function describeQualities(): Promise<readonly PageVariantOption[]> {
    const value = doneValue(await player.call(BENCH_SERIES_HAND_ACTIONS.qualities, null), BENCH_SERIES_HAND_ACTIONS.qualities);
    if (!Array.isArray(value)) throw new BenchSeriesPageError("qualities is not a list");
    return (value as readonly HandValue[]).map(qualityOption);
  }

  async function describeNextEpisode(): Promise<boolean> {
    const value = doneValue(await player.call(BENCH_SERIES_HAND_ACTIONS.hasNextEpisode, null), BENCH_SERIES_HAND_ACTIONS.hasNextEpisode);
    if (typeof value !== "boolean") throw new BenchSeriesPageError("has_next_episode is not a boolean");
    return value;
  }

  /** Есть ли у озвучки серия, которая сейчас на странице. Нет списка серий у кнопки - есть во всех. */
  function dubHasEpisode(button: AdElement, where: AdElement): boolean {
    const episodes = dom.attributeOf(button, markup.dubEpisodesAttribute);
    if (episodes === null) return true;
    const current = dom.attributeOf(where, markup.episodeKeyAttribute);
    return current !== null && episodes.split(/\s+/u).includes(current);
  }

  /** Озвучка: найти кнопку перебором, а не селектором из `id` (id пришёл извне, в селектор его не подставляем). */
  function selectDub(id: string): VariantActionOutcome {
    const where = root();
    if (where === undefined) return "no_such_variant";
    const button = dubButtons(where).find((candidate) => dom.attributeOf(candidate, markup.dubKeyAttribute) === id);
    if (button === undefined) return "no_such_variant";
    // У озвучки нет текущей серии: не нажимаем, страница остаётся как была (как Ashdi, решение 295).
    if (!dubHasEpisode(button, where)) return "no_such_episode";
    button.click();
    return "done";
  }

  return {
    matches: (place: AdFramePlace) => markup.hosts.includes(place.hostname as (typeof markup.hosts)[number]) && place.contains(markup.rootSelector),
    observationRoot: () => root() ?? dom.documentRoot(),
    async describe(): Promise<PageVariantDescription | null> {
      const where = root();
      if (where === undefined) return null;
      const audio = dubButtons(where).flatMap((button) => dubOption(button) ?? []);
      const [quality, nextEpisode] = await Promise.all([describeQualities(), describeNextEpisode()]);
      return {
        seriesKey: dom.attributeOf(where, markup.seriesKeyAttribute),
        episodeKey: dom.attributeOf(where, markup.episodeKeyAttribute),
        audio,
        quality,
        subtitles: [], // стенд не предлагает язык субтитров: документ приходит дорожкой (13b)
        nextEpisode,
      };
    },
    async select(kind: PageVariantKind, id: string): Promise<VariantActionOutcome> {
      if (kind === "audio") return selectDub(id);
      return actionOutcome(await player.call(BENCH_SERIES_HAND_ACTIONS.selectQuality, id));
    },
    async next(): Promise<VariantActionOutcome> {
      return actionOutcome(await player.call(BENCH_SERIES_HAND_ACTIONS.nextEpisode, null));
    },
  };
}
