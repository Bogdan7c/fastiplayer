// Меню вариантов страницы на стороне сеанса (сессия 14b, решения 17, 19, 279–283): аренда наблюдения во
// фреймах, сводка, отправка меню плееру и исполнение его команд.
//
// Устроено как дублёр кнопки пропуска (`ad-skip-mirror.ts`): фон страницу не читает. Читают фреймы
// (адаптеры в изолированном мире), а фон держит аренду, собирает донесения и отвечает на команды плеера.
// До `videoStarted` и после `stop` фон и страница молчат (решение 198).
//
// Что уходит плееру:
// - `variant_menu_changed` (протокол 1.0) — меню озвучек и качеств, только при изменении; пустое меню —
//   «пунктов больше нет», уходит, только если раньше было непустое;
// - `page_navigation` (протокол 1.7) — «есть ли следующая серия», только при изменении и только плееру ≥ 1.7.
//   До первого слова плеер считает «нет», поэтому первое «нет» не шлётся.
// Название сериала, сезон и серия плееру не уходят (решение 283).
//
// Команды плеера:
// - `select_variant` идёт во фрейм, который прислал меню этого вида, и только если пункт в этом меню есть;
// - `next_episode` — во фрейм, который сказал «следующая серия есть».
// Исход команды (`person_only`, `no_such_variant`, `failed`) — строка в журнал разработчика: подсказку
// зрителю для `person_only` даст сессия 15 вместе с YouTube.

import {
  pageSessionIntroducedIn,
  versionIncludes,
  type NextEpisodeAvailability,
  type PageSessionMessage,
  type ProtocolVersion,
  type VariantMenu,
} from "../../protocol/index.ts";
import type { PageVariantKind, VariantActionOutcome, VariantCommand, VariantWatchReport } from "../../shared/variant-watch-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import { createVariantFrameSummary } from "./variant-frame-summary.ts";
import { normalizeVariantMenus } from "./variant-menu-normalize.ts";

/** Срок одной аренды наблюдения: три продления подряд могут потеряться. */
export const VARIANT_WATCH_LEASE_MS = 3_000;

/** Как часто фон продлевает аренду и пересчитывает сводку (фреймы, замолчавшие дольше срока, уходят). */
export const VARIANT_WATCH_RENEW_MS = 1_000;

/** Что нужно сеансу от браузера. */
export interface VariantMirrorBrowser {
  /** Взять наблюдение в аренду у всех фреймов вкладки. Сбой доставки не важен: продление повторит. */
  armVariantWatch(tabId: number, leaseMs: number): void;
  /** Попросить один фрейм исполнить команду; отказ доставки — исключение. */
  sendVariantCommand(tabId: number, frameId: number, command: VariantCommand): Promise<VariantActionOutcome>;
}

export interface SessionVariantMirrorDependencies {
  /** Вкладка страницы сеанса сейчас (после возврата страницы, 11b2, — другая). */
  readonly currentTabId: () => number;
  /** Фрейм видео сеанса: его меню главнее. */
  readonly sessionFrameId: () => number | undefined;
  readonly scheduler: Scheduler;
  readonly browser: VariantMirrorBrowser;
  /** Версия протокола, о которой договорились с плеером. */
  readonly agreedProtocol: () => ProtocolVersion;
  /** Отправить сообщение плееру (очередь сокета сеанса). */
  readonly send: (message: PageSessionMessage) => void;
  /** Отметка в журнал разработчика. */
  readonly note: (message: string) => void;
}

export interface SessionVariantMirror {
  /** Тень встала на ролик: начать аренду, если не начата, и пересчитать сводку. */
  videoStarted(): void;
  /** Донесение фрейма (форма проверена). Вне живого сеанса игнорируется. */
  acceptFrameReport(frameId: number, report: VariantWatchReport): void;
  /** Плеер просит выбрать пункт `variantId` вида `kind`. */
  selectVariant(kind: PageVariantKind, variantId: string): void;
  /** Плеер просит следующую серию. */
  nextEpisode(): void;
  /** Сеанс кончился: аренда не продлевается, донесения не принимаются. */
  stop(): void;
}

/** Что плеер уже знает от нас. */
interface SentState {
  menuKey: Record<PageVariantKind, string>;
  /** Фрейм, приславший меню вида, — ему и команды этого вида. */
  menuFrame: Partial<Record<PageVariantKind, number>>;
  menus: Record<PageVariantKind, VariantMenu>;
  nextEpisode: NextEpisodeAvailability;
  nextEpisodeFrame: number | undefined;
}

const EMPTY_KEY = "";
const PAGE_VARIANT_KINDS: readonly PageVariantKind[] = ["audio", "quality"];

export function createSessionVariantMirror(dependencies: SessionVariantMirrorDependencies): SessionVariantMirror {
  const { browser, currentTabId } = dependencies;
  const renewTimer = createTimerSlot(dependencies.scheduler);
  const summary = createVariantFrameSummary(() => dependencies.scheduler.now());
  const notedAdapters = new Set<string>();
  let running = false;
  const sent: SentState = {
    menuKey: { audio: EMPTY_KEY, quality: EMPTY_KEY },
    menuFrame: {},
    menus: { audio: { kind: "audio", options: [] }, quality: { kind: "quality", options: [] } },
    nextEpisode: "unavailable",
    nextEpisodeFrame: undefined,
  };

  /** Ключ сравнения меню: пустое меню — пустой ключ (плееру «ничего» до первого непустого не шлём). */
  const menuKeyOf = (menu: VariantMenu): string => (menu.options.length === 0 ? EMPTY_KEY : JSON.stringify(menu.options));

  /** Пересчитать главное меню и сказать плееру то, что изменилось. */
  function recompute(): void {
    const chosen = summary.current(dependencies.sessionFrameId());
    if (chosen !== undefined && !notedAdapters.has(chosen.adapter)) {
      notedAdapters.add(chosen.adapter);
      // Версия адаптера — в журнал разработчика (решение 283): без адресов и названий.
      dependencies.note(`variant adapter ${chosen.adapter} describes the page`);
    }
    const menus = normalizeVariantMenus(chosen?.description ?? null);
    for (const kind of PAGE_VARIANT_KINDS) {
      const menu = menus[kind];
      const key = menuKeyOf(menu);
      if (key === sent.menuKey[kind]) continue;
      sent.menuKey[kind] = key;
      sent.menus[kind] = menu;
      if (chosen !== undefined && key !== EMPTY_KEY) sent.menuFrame[kind] = chosen.frameId;
      else delete sent.menuFrame[kind];
      dependencies.send({ type: "variant_menu_changed", menu });
      // Отправка могла синхронно закончить сеанс (очередь сокета переполнена): дальше говорить некому.
      if (!running) return;
    }
    sendNavigation(menus.nextEpisode, chosen?.frameId);
  }

  function sendNavigation(nextEpisode: NextEpisodeAvailability, frameId: number | undefined): void {
    sent.nextEpisodeFrame = nextEpisode === "available" ? frameId : undefined;
    if (nextEpisode === sent.nextEpisode) return;
    sent.nextEpisode = nextEpisode;
    // Старый плеер не знает `page_navigation` и рвал бы сеанс: молчим, у него и кнопки Next нет.
    if (!versionIncludes(dependencies.agreedProtocol(), pageSessionIntroducedIn("page_navigation"))) return;
    dependencies.send({ type: "page_navigation", next_episode: nextEpisode });
  }

  function renew(): void {
    browser.armVariantWatch(currentTabId(), VARIANT_WATCH_LEASE_MS);
    recompute();
    if (running) renewTimer.start(VARIANT_WATCH_RENEW_MS, renew);
  }

  /** Отдать команду фрейму и записать исход в журнал. */
  function command(frameId: number, request: VariantCommand, what: string): void {
    browser.sendVariantCommand(currentTabId(), frameId, request).then(
      (outcome) => {
        if (outcome !== "done") dependencies.note(`${what} not done on the page: ${outcome}`);
      },
      (error: unknown) => dependencies.note(`${what} could not reach the frame: ${String(error)}`),
    );
  }

  return {
    videoStarted() {
      if (!running) {
        running = true;
        renew();
        return;
      }
      recompute();
    },
    acceptFrameReport(frameId, report) {
      if (!running) return;
      summary.accept(frameId, report);
      recompute();
    },
    selectVariant(kind, variantId) {
      if (!running) return;
      const frameId = sent.menuFrame[kind];
      // Пункт должен быть в последнем меню, которое видел плеер: устаревший выбор не нажимает чужое.
      if (frameId === undefined || !sent.menus[kind].options.some((option) => option.id === variantId)) {
        dependencies.note(`select_variant ignored: no such ${kind} option in the last menu`);
        return;
      }
      command(frameId, { kind: "select", variant_kind: kind, variant_id: variantId }, `select_variant ${kind}`);
    },
    nextEpisode() {
      if (!running) return;
      if (sent.nextEpisodeFrame === undefined) {
        dependencies.note("next_episode ignored: the page offers no next episode");
        return;
      }
      command(sent.nextEpisodeFrame, { kind: "next" }, "next_episode");
    },
    stop() {
      running = false;
      renewTimer.cancel();
    },
  };
}
