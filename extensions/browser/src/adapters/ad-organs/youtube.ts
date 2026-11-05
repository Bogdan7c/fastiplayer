// Органы рекламы YouTube. Факты разведки: настоящая мышь принимается, любой синтетический клик
// (`click()`, цепочка pointer/mouse-событий, `<label for>`) YouTube игнорирует (решение 207). Поэтому:
// - показанная кнопка пропуска — `on_page` («нажмёт человек на странице»), никогда не `available`;
// - числового отсчёта на странице нет, `countdown` не бывает;
// - скрытая кнопка — `unavailable`;
// - `pressSkip` ничего не нажимает: показанная кнопка — `person_only`, нет кнопки — `no_button`.

import type { PageAdState } from "../../protocol/index.ts";
import { adPodFromCounterText, hostMatches } from "./ad-text.ts";
import type { AdElement, AdFramePlace, AdOrgans, AdPageView, PressSkipOutcome } from "./contract.ts";
import { YOUTUBE_ORGANS } from "./selectors.ts";

export function createYouTubeOrgans(view: AdPageView): AdOrgans {
  /** Плеер, если в нём идёт реклама. */
  function playerShowingAd(): AdElement | undefined {
    const player = view.find(YOUTUBE_ORGANS.playerSelector);
    return player !== undefined && view.hasClass(player, YOUTUBE_ORGANS.adShowingClass) ? player : undefined;
  }

  /** Кнопка пропуска, которую сайт сейчас показывает. */
  function shownSkipButton(player: AdElement): AdElement | undefined {
    const button = view.find(YOUTUBE_ORGANS.skipButtonSelector, player);
    return button !== undefined && view.isRendered(button) ? button : undefined;
  }

  return {
    family: "youtube",
    matches: (place: AdFramePlace) => hostMatches(place.hostname, YOUTUBE_ORGANS.hostSuffixes),
    observationRoot: () => view.find(YOUTUBE_ORGANS.playerSelector) ?? view.documentRoot(),
    readAdState(): PageAdState {
      const player = playerShowingAd();
      if (player === undefined) return { kind: "none" };
      const counter = view.find(YOUTUBE_ORGANS.podCounterSelector, player);
      return {
        kind: "showing",
        pod: counter === undefined ? { kind: "unknown" } : adPodFromCounterText(view.textOf(counter)),
        skip: shownSkipButton(player) === undefined ? { kind: "unavailable" } : { kind: "on_page" },
      };
    },
    pressSkip(): PressSkipOutcome {
      const player = playerShowingAd();
      // Кнопка видна, но клик скриптом сайт игнорирует: честно говорим «только человек», ничего не нажимая.
      return player !== undefined && shownSkipButton(player) !== undefined ? "person_only" : "no_button";
    },
  };
}
