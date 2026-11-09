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

/** Пункт качества из ответа страницы `{id, label, selected}`. */
function qualityOption(candidate: HandValue): PageVariantOption {
  if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) throw new BenchSeriesPageError("quality is not an object");
  const { id, label, selected } = candidate as Readonly<Record<string, HandValue>>;
  if (typeof id !== "string" || typeof label !== "string" || typeof selected !== "boolean") throw new BenchSeriesPageError("quality fields");
  return { id, label, selected };
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

  /** Озвучка: найти кнопку перебором, а не селектором из `id` (id пришёл извне, в селектор его не подставляем). */
  function selectDub(id: string): VariantActionOutcome {
    const where = root();
    const button = where === undefined ? undefined : dubButtons(where).find((candidate) => dom.attributeOf(candidate, markup.dubKeyAttribute) === id);
    if (button === undefined) return "no_such_variant";
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
