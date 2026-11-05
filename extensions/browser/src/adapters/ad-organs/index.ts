// Все органы рекламы, которые умеет читать экстеншен. Новый сайт — новый читатель в этом списке.

import type { AdOrgans, AdPageView } from "./contract.ts";
import { createImaOrgans } from "./ima.ts";
import { createPlayerJsOrgans } from "./playerjs.ts";
import { createYouTubeOrgans } from "./youtube.ts";

export type { AdElement, AdFramePlace, AdOrgans, AdPageView, ElementRect, PressSkipOutcome } from "./contract.ts";

/** Читатели всех семейств для документа, который показывает `view`. */
export function createAllAdOrgans(view: AdPageView): readonly AdOrgans[] {
  return [createYouTubeOrgans(view), createImaOrgans(view), createPlayerJsOrgans(view)];
}
