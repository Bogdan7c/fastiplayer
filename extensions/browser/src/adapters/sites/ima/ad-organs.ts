// Органы рекламы Google IMA SDK. Интерфейс рекламы живёт в чужом iframe `imasdk.googleapis.com`, поэтому
// читатель работает в том фрейме. Счётчика «N из M» в этом интерфейсе нет (`pod` всегда `unknown`),
// отсчёт — текст-число, кнопка принимает `click()` (факт разведки), значит показанная кнопка — `available`.

import type { PageAdState } from "../../../protocol/index.ts";
import { countdownFromText, hostMatches } from "../../capabilities/ad-organs/ad-text.ts";
import type { AdElement, AdFramePlace, AdOrgans, AdPageView, PressSkipOutcome } from "../../capabilities/ad-organs/contract.ts";
import { IMA_ORGANS } from "./ad-selectors.ts";

export function createImaOrgans(view: AdPageView): AdOrgans {
  /** Контейнер рекламы, если он показан. */
  function shownContainer(): AdElement | undefined {
    const container = view.find(IMA_ORGANS.containerSelector);
    return container !== undefined && view.isRendered(container) ? container : undefined;
  }

  function shownSkipButton(container: AdElement): AdElement | undefined {
    const button = view.find(IMA_ORGANS.skipButtonSelector, container);
    return button !== undefined && view.isRendered(button) ? button : undefined;
  }

  return {
    family: "ima",
    // Хост iframe SDK или IMA, нарисованный прямо в документе страницы.
    matches: (place: AdFramePlace) => hostMatches(place.hostname, IMA_ORGANS.hostSuffixes) || place.contains(IMA_ORGANS.containerSelector),
    observationRoot: () => view.find(IMA_ORGANS.containerSelector) ?? view.documentRoot(),
    readAdState(): PageAdState {
      const container = shownContainer();
      if (container === undefined) return { kind: "none" };
      if (shownSkipButton(container) !== undefined) return { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "available" } };
      const countdown = view.find(IMA_ORGANS.countdownSelector, container);
      const skip = countdown !== undefined && view.isRendered(countdown) ? countdownFromText(view.textOf(countdown)) : { kind: "unavailable" as const };
      return { kind: "showing", pod: { kind: "unknown" }, skip };
    },
    pressSkip(): PressSkipOutcome {
      const container = shownContainer();
      const button = container === undefined ? undefined : shownSkipButton(container);
      if (button === undefined) return "no_button";
      button.click();
      return "pressed";
    },
  };
}
