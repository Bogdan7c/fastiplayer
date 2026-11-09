// Память выбора одного сеанса передачи (сессия 14b2, решения 44, 281, 284, 285, 287).
//
// Что делает:
// - узнаёт сериал и серию по описанию страницы от зеркала вариантов (`variant-mirror.ts`, наблюдатель);
// - ЗАПИСЫВАЕТ выбор человека: пункт нашего меню (исход `done`), выбор субтитров в плеере
//   (`subtitle_choice`), смену озвучки или качества на самой странице не по нашей команде;
// - НЕ записывает: стартовое «по умолчанию» серии (первое увиденное — точка отсчёта), смену, которую сделала
//   сама память, побочные смены сразу после любой команды выбора (сайт мог сбросить качество при смене
//   озвучки), «Auto» качества сайта;
// - ПРИМЕНЯЕТ запомненное один раз на серию и не спорит со зрителем: что он выбрал в этой серии сам, память
//   уже не трогает. Озвучка — только у начала серии, качество — в любой момент (решение 287), субтитры —
//   подсказкой плееру `subtitle_preference` (протокол 1.7). В рекламе ничего не применяется (решение 283);
// - в инкогнито память читается, но не пишется, даже время касания (решение 285). Если браузер не сказал,
//   приватная ли вкладка, тоже не пишется.
//
// Выбор, сделанный в этом сеансе, действует до конца сеанса и в инкогнито: иначе следующая серия вернула бы
// зрителю старую озвучку поверх только что выбранной. В хранилище он уходит только из обычного окна.
//
// Ключи сериала и серии и подписи в журнал разработчика не пишутся.

import type { SubtitleChoice } from "../../protocol/index.ts";
import type { RememberedChoicePatch, RememberedChoices, SeriesFingerprint } from "../../shared/choice-memory-contract.ts";
import type { PageVariantKind, VariantActionOutcome } from "../../shared/variant-watch-contract.ts";
import type { DescribedVariantPage, RememberedSelectOutcome, VariantMirrorObserver, VariantSelectionOrigin } from "../handoff/variant-mirror.ts";
import type { ChoiceMemoryStore } from "./choice-memory-store.ts";
import { createPageTrackCatalog, type OfferedPageTrack } from "./page-track-catalog.ts";
import { planRememberedVariant, selectionOf, type EpisodeTiming, type SeenSelection } from "./remembered-choice-plan.ts";
import { browserSha256, seriesFingerprintOf, type Sha256Digest } from "./series-fingerprint.ts";

/** Числа поведения памяти. Не настройки человека: это допуски того, как ведут себя страницы и плеер. */
export const CHOICE_MEMORY_POLICY = {
  /** «Начало серии» для озвучки: первые столько секунд по часам плеера. */
  episodeStartLimitS: 30,
  /**
   * После любой команды выбора страница ещё перестраивается: смены выбора на странице в эти миллисекунды —
   * следствие команды, а не выбор человека.
   */
  settleAfterCommandMs: 2_000,
  /**
   * Дорожки субтитров новой серии ищутся среди предложенных с её начала; запас на то, что документ субтитров
   * пришёл чуть раньше донесения о новой серии (донесения сливаются раз в 250 мс и дольше).
   */
  episodeTrackGraceMs: 3_000,
} as const;

/** Можно ли писать в память в этом сеансе. */
export type ChoiceMemoryWriting = "allowed" | "private_window" | "privacy_unknown";

/** Исход подсказки субтитров плееру. */
export type SubtitlePreferenceDelivery = "sent" | "player_too_old";

export interface SessionChoiceMemoryDependencies {
  readonly store: ChoiceMemoryStore;
  /** Ключ сайта вкладки сейчас; `null` — не веб-страница (памяти нет). */
  readonly pageSiteKey: () => string | null;
  readonly writing: ChoiceMemoryWriting;
  /** Идёт ли сейчас ролик, принятый за рекламу. */
  readonly adOnScreen: () => boolean;
  /** Выбрать пункт на странице через ворота зеркала вариантов. */
  readonly selectOnPage: (kind: PageVariantKind, variantId: string) => RememberedSelectOutcome;
  /** Подсказать плееру запомненные субтитры. */
  readonly sendSubtitlePreference: (choice: SubtitleChoice) => SubtitlePreferenceDelivery;
  /** Часы фона (мс). */
  readonly now: () => number;
  readonly note: (message: string) => void;
  readonly digest?: Sha256Digest;
}

export interface SessionChoiceMemory extends VariantMirrorObserver {
  /** Часы плеера (`clock`): позиция в микросекундах. */
  playerClock(positionUs: number): void;
  /** Дорожку субтитров страницы предложили плееру. */
  subtitleTrackOffered(track: OfferedPageTrack): void;
  /** Зритель выбрал субтитры в плеере (`subtitle_choice`). */
  viewerSubtitleChoice(choice: SubtitleChoice): void;
  /** Сторона страницы сеанса остановлена (конец сеанса или страница ушла): память молчит. */
  stop(): void;
  /** Тень снова встала на ролик (в том числе в вернувшейся вкладке, 11b2): память снова слушает. */
  resume(): void;
}

/** Что память применяет за серию; каждое — один раз. */
type AppliedPart = PageVariantKind | "subtitles";

interface SeriesState {
  readonly seriesKey: string;
  readonly fingerprint: Promise<SeriesFingerprint | undefined>;
  /** Запись хранилища загружена (или её нет). До загрузки память ничего не применяет. */
  loaded: boolean;
  /** Что память знает о сериале: запись хранилища и поверх неё выбор этого сеанса. */
  choices: RememberedChoicePatch | undefined;
}

interface EpisodeState {
  readonly key: string | null;
  /** Серия, в которой сеанс начался: только у неё позиция может быть уже далеко от начала. */
  readonly first: boolean;
  readonly startedAt: number;
  /** Последний увиденный выбор страницы по видам; первое увиденное — точка отсчёта, а не выбор человека. */
  readonly seen: Partial<Record<PageVariantKind, SeenSelection>>;
  /** Что память на этой серии уже сделала или решила не делать (в том числе потому, что выбрал зритель). */
  readonly settled: Set<AppliedPart>;
}

const VARIANT_KINDS: readonly PageVariantKind[] = ["audio", "quality"];

export function createSessionChoiceMemory(dependencies: SessionChoiceMemoryDependencies): SessionChoiceMemory {
  const { store, note } = dependencies;
  const digest = dependencies.digest ?? browserSha256;
  const catalog = createPageTrackCatalog();
  let stopped = false;
  let series: SeriesState | undefined;
  let episode: EpisodeState | undefined;
  let lastPage: DescribedVariantPage | null = null;
  let playerSecond: number | undefined;
  let commandsInFlight = 0;
  let settledAt = Number.NEGATIVE_INFINITY;

  /** Команда выбора в пути или только что кончилась: смены на странице — её следствие. */
  function pageIsSettling(): boolean {
    return commandsInFlight > 0 || dependencies.now() < settledAt + CHOICE_MEMORY_POLICY.settleAfterCommandMs;
  }

  function startSeries(seriesKey: string): SeriesState {
    const siteKey = dependencies.pageSiteKey();
    const fingerprint =
      siteKey === null
        ? Promise.resolve(undefined)
        : seriesFingerprintOf(siteKey, seriesKey, digest).catch((error: unknown) => {
            note(`choice memory is off for this series: fingerprint failed: ${String(error)}`);
            return undefined;
          });
    const state: SeriesState = { seriesKey, fingerprint, loaded: false, choices: undefined };
    void loadSeries(state);
    return state;
  }

  async function loadSeries(state: SeriesState): Promise<void> {
    const fingerprint = await state.fingerprint;
    let stored: RememberedChoices | undefined;
    if (fingerprint !== undefined) {
      try {
        stored = await store.recall(fingerprint);
        // Применение — тоже касание: сериал, который смотрят, не вытесняется. В инкогнито — только чтение.
        if (stored !== undefined && dependencies.writing === "allowed") await store.touch(fingerprint);
      } catch (error) {
        note(`choice memory could not be read: ${String(error)}`);
      }
    }
    if (series !== state) return; // сериал сменился, пока читали
    const { used_at: _usedAt, ...remembered } = stored ?? { used_at: 0 };
    // Выбор этого сеанса, сделанный до конца чтения, главнее записи хранилища.
    state.choices = stored === undefined && state.choices === undefined ? undefined : { ...remembered, ...state.choices };
    state.loaded = true;
    if (stored !== undefined) note("choice memory found remembered choices for this series");
    applyRemembered();
  }

  function startEpisode(key: string | null, first: boolean): void {
    episode = { key, first, startedAt: dependencies.now(), seen: {}, settled: new Set() };
  }

  /** Где мы в серии для правила озвучки. Не первая серия сеанса открыта страницей с начала. */
  function episodeTiming(current: EpisodeState): EpisodeTiming {
    if (!current.first) return "near_start";
    if (playerSecond === undefined) return "position_unknown";
    return playerSecond <= CHOICE_MEMORY_POLICY.episodeStartLimitS ? "near_start" : "under_way";
  }

  /** Записать выбор человека: в сеанс всегда, в хранилище — только из обычного окна. */
  function rememberChoice(patch: RememberedChoicePatch, what: string): void {
    const current = series;
    if (current === undefined) {
      note(`choice not remembered (${what}): the page has no series`);
      return;
    }
    current.choices = { ...current.choices, ...patch };
    if (dependencies.writing !== "allowed") {
      note(`choice not remembered (${what}): ${dependencies.writing}`);
      return;
    }
    void current.fingerprint
      .then((fingerprint) => (fingerprint === undefined ? undefined : store.remember(fingerprint, patch).then(() => note(`choice remembered (${what})`))))
      .catch((error: unknown) => note(`choice could not be remembered (${what}): ${String(error)}`));
  }

  /** Сравнить выбор страницы с прошлым: смена не по нашей команде — выбор человека на странице (решение 284). */
  function observeSelections(page: DescribedVariantPage, current: EpisodeState): void {
    for (const kind of VARIANT_KINDS) {
      const selected = selectionOf(page.menus[kind], page.menus.automatic[kind]);
      if (selected === undefined) continue;
      const previous = current.seen[kind];
      current.seen[kind] = selected;
      if (previous === undefined || previous.label === selected.label || pageIsSettling()) continue;
      if (selected.automatic) {
        note(`page switched ${kind} to the site's automatic choice: not remembered`);
        continue;
      }
      // Человек выбрал на странице: память на этой серии этот вид больше не трогает.
      current.settled.add(kind);
      rememberChoice({ [kind]: selected.label }, `${kind} chosen on the page`);
    }
  }

  /** Применить запомненное, что ещё можно применить на этой серии. Повторный вызов безопасен. */
  function applyRemembered(): void {
    const current = episode;
    const page = lastPage;
    if (stopped || series === undefined || !series.loaded || series.choices === undefined || current === undefined || page === null) return;
    if (dependencies.adOnScreen()) return;
    applyRememberedSubtitles(series.choices, current);
    // Озвучка, потом качество, по одной команде за раз: смена озвучки на сайте может сбросить качество.
    if (pageIsSettling()) return;
    for (const kind of VARIANT_KINDS) {
      if (current.settled.has(kind)) continue;
      const step = planRememberedVariant({
        variantKind: kind,
        rememberedLabel: series.choices[kind],
        selected: current.seen[kind],
        menu: page.menus[kind],
        timing: episodeTiming(current),
      });
      if (step.kind === "wait") return;
      current.settled.add(kind);
      if (step.kind === "settled") {
        if (step.reason === "episode_under_way") note(`remembered ${kind} not applied: the episode is already under way`);
        if (step.reason === "not_on_page") note(`remembered ${kind} not applied: no such option on this page`);
        continue;
      }
      const outcome = dependencies.selectOnPage(kind, step.variantId);
      note(`remembered ${kind} applied on the page: ${outcome}`);
      if (outcome === "sent") return;
    }
  }

  function applyRememberedSubtitles(choices: RememberedChoicePatch, current: EpisodeState): void {
    const remembered = choices.subtitles;
    if (remembered === undefined || current.settled.has("subtitles")) return;
    let choice: SubtitleChoice;
    if (remembered.kind === "off") {
      choice = { kind: "off" };
    } else {
      const track = catalog.find(remembered, current.startedAt - CHOICE_MEMORY_POLICY.episodeTrackGraceMs);
      if (track === undefined) return; // дорожка серии ещё не пришла
      choice = { kind: "page_track", track };
    }
    current.settled.add("subtitles");
    note(`remembered subtitles suggested to the player: ${dependencies.sendSubtitlePreference(choice)}`);
  }

  return {
    pageDescribed(page) {
      if (stopped) return;
      lastPage = page;
      // Меню пропало (сбой адаптера, фрейм замолчал) — это не новая серия и не выбор.
      if (page === null) return;
      if (page.seriesKey === null) {
        series = undefined;
        return;
      }
      if (series?.seriesKey !== page.seriesKey) {
        const firstEpisode = episode === undefined;
        series = startSeries(page.seriesKey);
        startEpisode(page.episodeKey, firstEpisode);
      } else if (episode === undefined || (page.episodeKey !== null && page.episodeKey !== episode.key)) {
        startEpisode(page.episodeKey, episode === undefined);
      }
      if (episode !== undefined) observeSelections(page, episode);
      applyRemembered();
    },
    selectionStarted() {
      commandsInFlight += 1;
    },
    selectionFinished(kind: PageVariantKind, origin: VariantSelectionOrigin, outcome: VariantActionOutcome, label: string) {
      commandsInFlight = Math.max(0, commandsInFlight - 1);
      settledAt = dependencies.now();
      if (stopped || origin !== "viewer" || outcome !== "done") return;
      // Зритель выбрал в нашем меню: память на этой серии этот вид больше не трогает.
      episode?.settled.add(kind);
      rememberChoice({ [kind]: label }, `${kind} chosen in the player`);
    },
    playerClock(positionUs) {
      // Секунду помним и когда память молчит (страница ушла «сиротой», 11b2): вернувшейся странице она нужна сразу.
      playerSecond = Math.max(0, positionUs / 1_000_000);
      if (!stopped) applyRemembered();
    },
    subtitleTrackOffered(track) {
      if (stopped) return;
      catalog.offered(track, dependencies.now());
      applyRemembered();
    },
    viewerSubtitleChoice(choice) {
      if (stopped) return;
      episode?.settled.add("subtitles");
      if (choice.kind === "off") return rememberChoice({ subtitles: { kind: "off" } }, "subtitles turned off in the player");
      const track = catalog.describe(choice.track);
      if (track === undefined || (track.language === null && track.label === null)) {
        note("subtitle choice not remembered: the track has neither a language nor a label");
        return;
      }
      rememberChoice({ subtitles: { kind: "track", language: track.language, label: track.label } }, "subtitles chosen in the player");
    },
    stop() {
      stopped = true;
    },
    resume() {
      stopped = false;
    },
  };
}
