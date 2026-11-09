// Маленькая подделка DOM для читателей рекламы: дерево узлов и взгляд `AdPageView` над ним.
// Селекторы понимает только простые составные вида `tag#id.class.class[attribute]`, `a b` не поддерживаются:
// читатели ими и ограничены (см. src/adapters/sites/*/ad-selectors.ts).
import type { AdElement, AdPageView, ElementRect } from "../../src/adapters/capabilities/ad-organs/contract.ts";

export interface FakeNodeOptions {
  readonly id?: string;
  readonly classes?: readonly string[];
  readonly text?: string;
  readonly rect?: ElementRect;
  /** `false` — как `display:none`: не показан сам (и все потомки). */
  readonly rendered?: boolean;
  /** Атрибуты узла (адаптеры вариантов, 14b). */
  readonly attributes?: Readonly<Record<string, string>>;
}

export class FakeNode implements AdElement {
  readonly tag: string;
  readonly id: string | undefined;
  readonly classes: Set<string>;
  readonly children: FakeNode[] = [];
  parent: FakeNode | undefined;
  text: string;
  rect: ElementRect;
  rendered: boolean;
  readonly attributes: Map<string, string>;
  /** Сколько раз по узлу кликнули. */
  clicks = 0;

  constructor(tag: string, options: FakeNodeOptions = {}) {
    this.tag = tag;
    this.id = options.id;
    this.classes = new Set(options.classes ?? []);
    this.text = options.text ?? "";
    this.rect = options.rect ?? { left: 10, top: 10, right: 110, bottom: 30 };
    this.rendered = options.rendered ?? true;
    this.attributes = new Map(Object.entries(options.attributes ?? {}));
  }

  add(...nodes: FakeNode[]): this {
    for (const node of nodes) {
      node.parent = this;
      this.children.push(node);
    }
    return this;
  }

  click(): void {
    this.clicks += 1;
  }

  textContent(): string {
    return this.text + this.children.map((child) => child.textContent()).join("");
  }

  isRenderedWithAncestors(): boolean {
    return this.rendered && (this.parent?.isRenderedWithAncestors() ?? true);
  }

  *descendants(): Generator<FakeNode> {
    for (const child of this.children) {
      yield child;
      yield* child.descendants();
    }
  }

  matches(selector: string): boolean {
    const match = /^([a-z]*)(?:#([\w-]+))?((?:\.[\w-]+)*)(?:\[([\w-]+)\])?$/u.exec(selector);
    if (match === null) throw new Error(`fake DOM does not understand selector: ${selector}`);
    const [, tag = "", id, classes = "", attribute] = match;
    if (tag !== "" && tag !== this.tag) return false;
    if (id !== undefined && id !== this.id) return false;
    if (attribute !== undefined && !this.attributes.has(attribute)) return false;
    return classes
      .split(".")
      .filter((name) => name !== "")
      .every((name) => this.classes.has(name));
  }
}

export interface FakeViewOptions {
  readonly hostname?: string;
  readonly viewport?: { readonly width: number; readonly height: number };
}

/** Взгляд читателей на дерево `root` (корень документа). */
export function createFakeAdView(root: FakeNode, options: FakeViewOptions = {}): AdPageView {
  const all = (within: AdElement | undefined): FakeNode[] => [...((within as FakeNode | undefined) ?? root).descendants()];
  const find = (selector: string, within?: AdElement): FakeNode | undefined => all(within).find((node) => node.matches(selector));
  return {
    hostname: options.hostname ?? "www.example.com",
    contains: (selector) => find(selector) !== undefined,
    documentRoot: () => root,
    find,
    findAll: (selector, within) => all(within).filter((node) => node.matches(selector)),
    textOf: (element) => (element as FakeNode).textContent(),
    hasClass: (element, className) => (element as FakeNode).classes.has(className),
    hasChildElements: (element) => (element as FakeNode).children.length > 0,
    isRendered: (element) => (element as FakeNode).isRenderedWithAncestors(),
    rectOf: (element) => (element as FakeNode).rect,
    viewportSize: () => options.viewport ?? { width: 1280, height: 720 },
  };
}

// ------------------------------------------------------------------ готовые страницы (факты разведки 09d)

export interface YouTubePage {
  readonly root: FakeNode;
  readonly player: FakeNode;
  readonly counter: FakeNode | undefined;
  readonly skipButton: FakeNode | undefined;
}

/** Плеер YouTube: класс `ad-showing`, счётчик «1 из 2», кнопка пропуска (скрыта или показана). */
export function youTubePage(options: { adShowing: boolean; counterText?: string; skip: "hidden" | "shown" | "none" }): YouTubePage {
  const root = new FakeNode("html");
  const player = new FakeNode("div", { id: "movie_player", classes: options.adShowing ? ["html5-video-player", "ad-showing"] : ["html5-video-player"] });
  root.add(player);
  const counter = options.counterText === undefined ? undefined : new FakeNode("div", { classes: ["ytp-ad-pod-index"], text: options.counterText });
  if (counter !== undefined) player.add(counter);
  const skipButton = options.skip === "none" ? undefined : new FakeNode("button", { classes: ["ytp-skip-ad-button"], text: "Пропустить", rendered: options.skip === "shown" });
  if (skipButton !== undefined) player.add(skipButton);
  return { root, player, counter, skipButton };
}

export interface ImaPage {
  readonly root: FakeNode;
  readonly container: FakeNode;
  readonly countdown: FakeNode | undefined;
  readonly skipButton: FakeNode | undefined;
}

/** Интерфейс IMA: `.videoAdUi`, отсчёт-число, кнопка `button.videoAdUiSkipButton`. */
export function imaPage(options: { countdownText?: string; skip: "shown" | "hidden" | "none" }): ImaPage {
  const root = new FakeNode("html");
  const container = new FakeNode("div", { classes: ["videoAdUi"] });
  root.add(container);
  const countdown = options.countdownText === undefined ? undefined : new FakeNode("div", { classes: ["videoAdUiPreSkipText"], text: options.countdownText });
  if (countdown !== undefined) container.add(countdown);
  let skipButton: FakeNode | undefined;
  if (options.skip !== "none") {
    const skipContainer = new FakeNode("div", { classes: ["videoAdUiSkipContainer"], rendered: options.skip === "shown" });
    skipButton = new FakeNode("button", { classes: ["videoAdUiSkipButton"], text: "Skip ad" });
    skipContainer.add(skipButton);
    container.add(skipContainer);
  }
  return { root, container, countdown, skipButton };
}

export interface PlayerJsLeafSpec {
  readonly text: string;
  readonly rect?: ElementRect;
  readonly rendered?: boolean;
}

export interface PlayerJsPage {
  readonly root: FakeNode;
  readonly leaves: readonly FakeNode[];
}

/** PlayerJS: вложенные `pjsdiv` без классов; листья — по описаниям. */
export function playerJsPage(leafSpecs: readonly PlayerJsLeafSpec[]): PlayerJsPage {
  const root = new FakeNode("html");
  const outer = new FakeNode("pjsdiv");
  const inner = new FakeNode("pjsdiv");
  root.add(outer);
  outer.add(inner);
  const leaves = leafSpecs.map((spec) => {
    const leaf = new FakeNode("pjsdiv", { text: spec.text, ...(spec.rect === undefined ? {} : { rect: spec.rect }), ...(spec.rendered === undefined ? {} : { rendered: spec.rendered }) });
    inner.add(leaf);
    return leaf;
  });
  return { root, leaves };
}

/** Лист за экраном, как невидимое меню PlayerJS (y = −2000). */
export const OFFSCREEN_RECT: ElementRect = { left: 10, top: -2000, right: 110, bottom: -1980 };
