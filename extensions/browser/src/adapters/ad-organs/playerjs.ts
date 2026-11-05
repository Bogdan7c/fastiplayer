// Органы рекламы PlayerJS. Разметка без классов и id, поэтому всё различается по ТЕКСТУ ЛИСТЬЕВ дерева
// `<pjsdiv>`. Берём только листья, которые показаны и лежат внутри вьюпорта: у плеера есть невидимые
// листья меню за экраном (y = −2000: «Субтитри», «Швидкість»), их считать нельзя.
//
// Что значит «идёт реклама»: виден лист-счётчик («Реклама 1/2») или лист-отсчёт («Пропустити через 15»).
// Одно слово «Пропустити» без них рекламой не считается: оно может оказаться чем угодно на экране фильма.
// `click()` по листу принимается (факт разведки), значит кнопка — `available`.

import type { PageAdState } from "../../protocol/index.ts";
import { adPodFromNumbers, countdownFromSeconds, normalizeText } from "./ad-text.ts";
import type { AdElement, AdFramePlace, AdOrgans, AdPageView, PressSkipOutcome } from "./contract.ts";
import { PLAYERJS_ORGANS } from "./selectors.ts";

/** Регулярное выражение из слов-данных; слова — только буквы и пробелы, но экранируем на всякий случай. */
function alternation(words: readonly string[]): string {
  return words.map((word) => word.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&")).join("|");
}

const COUNTER_PATTERN = new RegExp(`^(?:${alternation(PLAYERJS_ORGANS.counterWords)})\\s*(\\d+)\\s*/\\s*(\\d+)$`, "u");
const COUNTDOWN_PATTERN = new RegExp(
  `^(?:${alternation(PLAYERJS_ORGANS.countdownPhrases)})\\s*(\\d+)\\s*(?:${alternation(PLAYERJS_ORGANS.countdownUnits)})?\\.?$`,
  "u",
);
const SKIP_PATTERN = new RegExp(`^(?:${alternation(PLAYERJS_ORGANS.skipPhrases)})(?:\\s*[›»>→])?$`, "u");

/** Числа в тексте листа не длиннее лимита цифр: длиннее — мусор. */
const MAX_NUMBER_DIGITS = 6;

function numberOf(digits: string | undefined): number | undefined {
  return digits !== undefined && digits.length <= MAX_NUMBER_DIGITS ? Number(digits) : undefined;
}

/** Что листья сказали о рекламе. */
interface LeafReading {
  readonly counter: RegExpMatchArray | undefined;
  readonly countdown: RegExpMatchArray | undefined;
  readonly skipLeaf: AdElement | undefined;
}

export function createPlayerJsOrgans(view: AdPageView): AdOrgans {
  /** Видимые листья внутри вьюпорта вместе с нормализованным текстом (без регистра). */
  function visibleLeafTexts(): { readonly leaf: AdElement; readonly text: string }[] {
    const viewport = view.viewportSize();
    const found: { leaf: AdElement; text: string }[] = [];
    for (const element of view.findAll(PLAYERJS_ORGANS.rootSelector)) {
      if (view.hasChildElements(element) || !view.isRendered(element)) continue;
      const rect = view.rectOf(element);
      const insideViewport = rect.right > 0 && rect.bottom > 0 && rect.left < viewport.width && rect.top < viewport.height;
      if (!insideViewport) continue;
      const text = normalizeText(view.textOf(element)).toLocaleLowerCase();
      if (text.length > 0) found.push({ leaf: element, text });
    }
    return found;
  }

  function readLeaves(): LeafReading {
    let counter: RegExpMatchArray | undefined;
    let countdown: RegExpMatchArray | undefined;
    let skipLeaf: AdElement | undefined;
    for (const { leaf, text } of visibleLeafTexts()) {
      counter ??= COUNTER_PATTERN.exec(text) ?? undefined;
      countdown ??= COUNTDOWN_PATTERN.exec(text) ?? undefined;
      if (skipLeaf === undefined && SKIP_PATTERN.test(text)) skipLeaf = leaf;
    }
    return { counter, countdown, skipLeaf };
  }

  return {
    family: "playerjs",
    // Разметка PlayerJS строится скриптом сайта, адреса у неё общего нет: признак — свой корень в документе.
    matches: (place: AdFramePlace) => place.contains(PLAYERJS_ORGANS.rootSelector),
    // Весь документ: первый `<pjsdiv>` может быть узкой веткой, а счётчик и кнопка лежат в других.
    // Дорого это не будет: наблюдатель читает только пока идёт реклама (`content/ad-watch.ts`).
    observationRoot: () => view.documentRoot(),
    readAdState(): PageAdState {
      const { counter, countdown, skipLeaf } = readLeaves();
      if (counter === undefined && countdown === undefined) return { kind: "none" };
      // Реклама идёт. Номер — числа сайта как есть (на второй рекламе сайт всё равно пишет 1/2).
      const pod = counter === undefined ? ({ kind: "unknown" } as const) : adPodFromNumbers(numberOf(counter[1]), numberOf(counter[2]));
      if (skipLeaf !== undefined) return { kind: "showing", pod, skip: { kind: "available" } };
      const skip = countdown === undefined ? ({ kind: "unavailable" } as const) : countdownFromSeconds(numberOf(countdown[1]));
      return { kind: "showing", pod, skip };
    },
    pressSkip(): PressSkipOutcome {
      const { counter, countdown, skipLeaf } = readLeaves();
      // Нажимаем, только пока сайт показывает рекламу: одинокое «Пропустити» чужим элементом не трогаем.
      if (skipLeaf === undefined || (counter === undefined && countdown === undefined)) return "no_button";
      skipLeaf.click();
      return "pressed";
    },
  };
}
