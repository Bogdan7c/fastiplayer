// Адаптер вариантов Twitch (изолированный мир): только качество эфира.
//
// - Озвучек у Twitch нет, следующей серии нет (`next()` страницу не трогает).
// - seriesKey — постоянная строка сайта (решение 289: память одна на весь сайт), episodeKey — логин канала.
// - Плеера на странице нет (ещё строится) — `describe()` отвечает `null`: меню у плеера не будет, это не сбой.
//   Любое другое неожиданное поведение страницы (рука упала, мусор в ответе) — исключение: описание целиком
//   либо верное, либо никакое.
// - Вшитая реклама Twitch кнопки пропуска не имеет — здесь ей заняться нечем.

import type { AdFramePlace } from "../../capabilities/ad-organs/contract.ts";
import { hostMatches } from "../../capabilities/ad-organs/ad-text.ts";
import type {
  PageVariantDescription,
  PageVariantKind,
  PageVariantOption,
  VariantActionOutcome,
  VariantAdapter,
  VariantHands,
} from "../../capabilities/variants/contract.ts";
import { withResolutionStepLabels } from "../../capabilities/variants/quality-label.ts";
import type { HandValue } from "../../../shared/adapter-hand-contract.ts";
import { TWITCH_SITE } from "./identity.ts";
import { TWITCH_HAND_ACTIONS, TWITCH_MARKUP } from "./markup.ts";

/** Плеер страницы ответил не так, как ждёт адаптер. */
export class TwitchPageError extends Error {}

function isRecord(candidate: HandValue | undefined): candidate is Readonly<Record<string, HandValue>> {
  return typeof candidate === "object" && candidate !== null && !Array.isArray(candidate);
}

function qualityOption(candidate: HandValue): PageVariantOption {
  if (!isRecord(candidate)) throw new TwitchPageError("quality is not an object");
  const { id, label, selected, automatic } = candidate;
  if (typeof id !== "string" || typeof label !== "string" || typeof selected !== "boolean") throw new TwitchPageError("quality fields");
  return automatic === true ? { id, label, selected, automatic } : { id, label, selected };
}

export function createTwitchVariants(hands: VariantHands): VariantAdapter {
  const { dom, player } = hands;
  return {
    matches: (place: AdFramePlace) => hostMatches(place.hostname, TWITCH_MARKUP.hostSuffixes) && place.contains(TWITCH_MARKUP.playerElementSelector),
    observationRoot: () => dom.find(TWITCH_MARKUP.playerElementSelector) ?? dom.documentRoot(),
    async describe(): Promise<PageVariantDescription | null> {
      const result = await player.call(TWITCH_HAND_ACTIONS.describe, null);
      // Рука сказала «плеера нет» — страница не готова, меню не будет.
      if (result.kind === "no_such_action") return null;
      if (result.kind !== "done") throw new TwitchPageError(`${TWITCH_HAND_ACTIONS.describe}: ${result.kind}`);
      if (!isRecord(result.value)) throw new TwitchPageError("description is not an object");
      const { episodeKey, quality } = result.value;
      if (!Array.isArray(quality)) throw new TwitchPageError("quality is not a list");
      if (episodeKey !== null && typeof episodeKey !== "string") throw new TwitchPageError("episodeKey");
      return {
        seriesKey: TWITCH_SITE.id,
        episodeKey,
        audio: [],
        // Подпись — ступень («720p», а не «720p60»): одинакова на всех каналах, память выбора её найдёт.
        quality: withResolutionStepLabels((quality as readonly HandValue[]).map(qualityOption)),
        nextEpisode: false,
      };
    },
    async select(kind: PageVariantKind, id: string): Promise<VariantActionOutcome> {
      // Озвучек нет; id пункта протокола не подставляем никуда, кроме сравнения в MAIN-мире.
      if (kind !== "quality" || !TWITCH_MARKUP.optionIdPattern.test(id)) return "no_such_variant";
      const result = await player.call(TWITCH_HAND_ACTIONS.selectQuality, id);
      if (result.kind !== "done") return "failed";
      if (result.value === true) return "done";
      return result.value === false ? "no_such_variant" : "failed";
    },
    // У эфира следующей серии нет: страницу не трогаем.
    next: async () => "no_such_variant",
  };
}
