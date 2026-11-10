// Память выбора одного сеанса (14b2, решения 284, 285, 287): что записывается, что нет, что и когда
// применяется. Настоящее хранилище памяти на хранилище в памяти, страница — описания, как их даёт зеркало.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { describe, it } from "node:test";
import { createChoiceMemoryStore } from "../../../src/background/choice-memory/choice-memory-store.ts";
import { seriesFingerprintOf } from "../../../src/background/choice-memory/series-fingerprint.ts";
import {
  CHOICE_MEMORY_POLICY,
  createSessionChoiceMemory,
  type ChoiceMemoryWriting,
} from "../../../src/background/choice-memory/session-choice-memory.ts";
import type { DescribedVariantPage } from "../../../src/background/handoff/variant-mirror.ts";
import { normalizeVariantMenus } from "../../../src/background/handoff/variant-menu-normalize.ts";
import type { SubtitleChoice } from "../../../src/protocol/index.ts";
import { REMEMBERED_CHOICES_KEY, type RememberedChoices } from "../../../src/shared/choice-memory-contract.ts";
import type { PageVariantKind } from "../../../src/shared/variant-watch-contract.ts";
import { memoryStore } from "../auto-handoff-lists-fake-dom.ts";

const SITE = "example.com";
const SERIES = "some-show";
const FINGERPRINT = await seriesFingerprintOf(SITE, SERIES);
const DUBS = ["Dub A", "Dub B", "Dub C"] as const;
const QUALITIES = ["Auto", "360p", "720p"] as const;

/**
 * Тот же SHA-256, но без пула потоков: `crypto.subtle` под нагрузкой параллельного прогона отвечает через
 * непредсказуемое число кругов цикла событий, а тесту нужен детерминированный порядок.
 */
const syncSha256 = (bytes: Uint8Array): Promise<ArrayBuffer> => {
  const hash = createHash("sha256").update(bytes).digest();
  return Promise.resolve(hash.buffer.slice(hash.byteOffset, hash.byteOffset + hash.byteLength) as ArrayBuffer);
};

/** Описание страницы, как его даёт зеркало: выбранные подписи, серия, «Auto» у первого качества. */
function page(dub: (typeof DUBS)[number], quality: (typeof QUALITIES)[number], episodeKey: string | null = "1", seriesKey: string | null = SERIES): DescribedVariantPage {
  const menus = normalizeVariantMenus({
    seriesKey,
    episodeKey,
    audio: DUBS.map((label, index) => ({ id: `dub-${index}`, label, selected: label === dub })),
    quality: QUALITIES.map((label, index) => ({ id: `q-${index}`, label, selected: label === quality, ...(index === 0 ? { automatic: true } : {}) })),
    nextEpisode: true,
  });
  return { seriesKey, episodeKey, menus };
}

const idOf = (kind: PageVariantKind, label: string): string =>
  kind === "audio" ? `dub-${DUBS.indexOf(label as (typeof DUBS)[number])}` : `q-${QUALITIES.indexOf(label as (typeof QUALITIES)[number])}`;

async function settle(): Promise<void> {
  for (let round = 0; round < 10; round += 1) await new Promise((resolve) => setImmediate(resolve));
}

interface HarnessOptions {
  readonly remembered?: Omit<RememberedChoices, "used_at">;
  readonly writing?: ChoiceMemoryWriting;
  readonly siteKey?: string | null;
}

function harness(options: HarnessOptions = {}) {
  const seed = options.remembered === undefined ? {} : { [REMEMBERED_CHOICES_KEY]: { version: 1, series: { [FINGERPRINT]: { ...options.remembered, used_at: 7 } } } };
  const storage = memoryStore(seed);
  const clock = { now: 100_000 };
  const store = createChoiceMemoryStore({ store: storage.store, now: () => clock.now });
  const selects: string[] = [];
  const preferences: SubtitleChoice[] = [];
  const notes: string[] = [];
  const ad = { onScreen: false };
  const memory = createSessionChoiceMemory({
    store,
    pageSiteKey: () => (options.siteKey === undefined ? SITE : options.siteKey),
    writing: options.writing ?? "allowed",
    adOnScreen: () => ad.onScreen,
    selectOnPage: (kind, variantId) => {
      selects.push(`${kind}:${variantId}`);
      return "sent";
    },
    sendSubtitlePreference: (choice) => {
      preferences.push(choice);
      return "sent";
    },
    now: () => clock.now,
    note: (message) => void notes.push(message),
    digest: syncSha256,
  });
  /** Что лежит в хранилище для нашего сериала. */
  const stored = (): RememberedChoices | undefined =>
    (storage.values.get(REMEMBERED_CHOICES_KEY) as { series: Record<string, RememberedChoices> } | undefined)?.series[FINGERPRINT];
  /** Команда памяти дошла до страницы и кончилась `done`; страница перестраивается. */
  const memoryCommandDone = (kind: PageVariantKind, label: string): void => {
    memory.selectionStarted(kind, "memory");
    memory.selectionFinished(kind, "memory", "done", label);
  };
  const afterSettle = (): void => void (clock.now += CHOICE_MEMORY_POLICY.settleAfterCommandMs + 1);
  return { memory, storage, clock, selects, preferences, notes, ad, stored, memoryCommandDone, afterSettle };
}

describe("что записывается", () => {
  it("выбор в нашем меню с исходом done — в хранилище подпись пункта; failed — ничего", async () => {
    const world = harness();
    world.memory.pageDescribed(page("Dub A", "360p"));
    world.memory.selectionStarted("quality", "viewer");
    world.memory.selectionFinished("quality", "viewer", "failed", "720p");
    await settle();
    assert.equal(world.stored(), undefined);
    world.memory.selectionStarted("audio", "viewer");
    world.memory.selectionFinished("audio", "viewer", "done", "Dub B");
    await settle();
    assert.deepEqual(world.stored(), { audio: "Dub B", used_at: 100_000 });
  });

  it("смена на странице не по нашей команде — выбор человека (решение 284); стартовое «по умолчанию» — нет", async () => {
    const world = harness();
    world.memory.pageDescribed(page("Dub A", "360p"));
    world.memory.playerClock(5_000_000, "playing"); // фильм серии на экране: дальше смены — человека
    await settle();
    assert.equal(world.stored(), undefined, "первое увиденное — точка отсчёта");
    world.memory.pageDescribed(page("Dub C", "360p"));
    await settle();
    assert.deepEqual(world.stored(), { audio: "Dub C", used_at: 100_000 });
  });

  it("смены сразу после команды (сайт сбросил качество при смене озвучки) и «Auto» сайта не записываются", async () => {
    const world = harness();
    world.memory.pageDescribed(page("Dub A", "720p"));
    world.memory.selectionStarted("audio", "viewer");
    world.memory.selectionFinished("audio", "viewer", "done", "Dub B");
    world.memory.pageDescribed(page("Dub B", "360p"));
    world.afterSettle();
    world.memory.pageDescribed(page("Dub B", "Auto"));
    await settle();
    assert.equal(world.stored()?.audio, "Dub B");
    assert.equal(world.stored()?.quality, undefined, "ни сброс после команды, ни «Auto» не стали выбором");
  });

  it("субтитры: дорожка из плеера помнится языком и подписью, «выключены» — как off", async () => {
    const world = harness();
    world.memory.pageDescribed(page("Dub A", "360p"));
    world.memory.subtitleTrackOffered({ track: 4, language: "uk", label: "Українська" });
    world.memory.viewerSubtitleChoice({ kind: "page_track", track: 4 });
    await settle();
    assert.deepEqual(world.stored()?.subtitles, { kind: "track", language: "uk", label: "Українська" });
    world.memory.viewerSubtitleChoice({ kind: "off" });
    await settle();
    assert.deepEqual(world.stored()?.subtitles, { kind: "off" });
    world.memory.viewerSubtitleChoice({ kind: "page_track", track: 99 });
    await settle();
    assert.deepEqual(world.stored()?.subtitles, { kind: "off" }, "неизвестная дорожка ничего не меняет");
  });

  it("у страницы нет сериала или это не веб-страница — ничего не пишется, отметка в журнале", async () => {
    const noSeries = harness();
    noSeries.memory.pageDescribed(page("Dub A", "360p", "1", null));
    noSeries.memory.selectionStarted("audio", "viewer");
    noSeries.memory.selectionFinished("audio", "viewer", "done", "Dub B");
    const noSite = harness({ siteKey: null });
    noSite.memory.pageDescribed(page("Dub A", "360p"));
    noSite.memory.selectionStarted("audio", "viewer");
    noSite.memory.selectionFinished("audio", "viewer", "done", "Dub B");
    await settle();
    assert.equal(noSeries.storage.state.writes + noSite.storage.state.writes, 0);
    assert.ok(noSeries.notes.some((note) => /no series/u.test(note)));
  });
});

describe("что и когда применяется", () => {
  it("у начала серии: сначала озвучка, после успокоения страницы — качество; повторные донесения ничего не жмут", async () => {
    const world = harness({ remembered: { audio: "Dub B", quality: "720p" } });
    world.memory.playerClock(1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub B")}`]);
    world.memoryCommandDone("audio", "Dub B");
    world.memory.pageDescribed(page("Dub B", "360p"));
    assert.equal(world.selects.length, 1, "пока страница перестраивается — ждём");
    world.afterSettle();
    world.memory.pageDescribed(page("Dub B", "360p"));
    assert.deepEqual(world.selects.at(-1), `quality:${idOf("quality", "720p")}`);
    world.memoryCommandDone("quality", "720p");
    world.afterSettle();
    world.memory.pageDescribed(page("Dub B", "720p"));
    world.memory.pageDescribed(page("Dub B", "720p"));
    assert.equal(world.selects.length, 2);
    await settle();
    assert.equal(world.stored()?.audio, "Dub B", "смены, сделанные памятью, — не новый выбор");
  });

  it("передали посреди серии: озвучку не трогаем (сайт начал бы с нуля), качество ставим (решение 287)", async () => {
    const world = harness({ remembered: { audio: "Dub B", quality: "720p" } });
    world.memory.playerClock(20 * 60 * 1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, [`quality:${idOf("quality", "720p")}`]);
    assert.ok(world.notes.some((note) => /remembered audio not applied: the episode is already under way/u.test(note)));
  });

  it("плеер ещё не прислал часы — ждать; часы пришли — применить", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, []);
    world.memory.playerClock(500_000, "playing");
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub B")}`]);
  });

  it("следующая серия, открытая страницей, — с начала: озвучка ставится, даже если часы ещё старой серии", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.memory.playerClock(20 * 60 * 1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p", "1"));
    await settle();
    assert.deepEqual(world.selects, []);
    world.memory.pageDescribed(page("Dub A", "360p", "2"));
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub B")}`]);
  });

  it("зритель выбрал в этой серии сам — память не спорит", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.memory.pageDescribed(page("Dub A", "360p"));
    // Фильм на экране, запись хранилища ещё читается: зритель успел выбрать на странице раньше памяти.
    world.memory.playerClock(1_000_000, "playing");
    world.memory.pageDescribed(page("Dub C", "360p"));
    await settle();
    assert.deepEqual(world.selects, []);
    assert.equal(world.stored()?.audio, "Dub C");
  });

  it("серия устраивается (15a, YouTube): смены страницы в рекламе и до показа нового поколения — не выбор, память цела и применяется", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.memory.pageDescribed(page("Dub A", "360p", "1"));
    world.memory.playerClock(5_000_000, "playing");
    await settle();
    world.selects.length = 0;
    // Next: новая серия, страница сначала показывает своё «по умолчанию», плеер ждёт новое поколение…
    world.memory.pageDescribed(page("Dub C", "360p", "2"));
    world.memory.playerClock(0, "waiting");
    // …в рекламе страница сама перескакивает на «свой» язык — это не человек.
    world.ad.onScreen = true;
    world.memory.pageDescribed(page("Dub A", "360p", "2"));
    world.ad.onScreen = false;
    world.memory.pageDescribed(page("Dub C", "360p", "2"));
    await settle();
    assert.equal(world.stored()?.audio, "Dub B", "запомненное не перезаписано языком страницы по умолчанию");
    assert.ok(world.notes.some((note) => /before the episode's film was on screen: not remembered/u.test(note)));
    // Фильм серии показан — память ставит запомненное (серия не первая: с начала).
    world.memory.playerClock(1_000_000, "playing");
    assert.deepEqual(world.selects.slice(-1), [`audio:${idOf("audio", "Dub B")}`]);
  });

  it("фильм серии на экране — смена на странице снова выбор человека, а реклама посреди фильма опять закрывает запись", async () => {
    const world = harness();
    world.memory.pageDescribed(page("Dub A", "360p"));
    world.memory.playerClock(10_000_000, "playing");
    world.ad.onScreen = true;
    world.memory.playerClock(0, "playing");
    world.memory.pageDescribed(page("Dub C", "360p"));
    await settle();
    assert.equal(world.stored(), undefined, "смена в рекламе не записана");
    world.ad.onScreen = false;
    world.memory.playerClock(11_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.equal(world.stored()?.audio, "Dub A", "после рекламы смена на странице — снова выбор человека");
  });

  it("передали в рекламе посреди серии: «0 / waiting» после рекламы — не начало серии, озвучка не трогается", async () => {
    const world = harness({ remembered: { audio: "Dub B", quality: "720p" } });
    world.ad.onScreen = true;
    world.memory.playerClock(15_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, [], "в рекламе — ничего");
    world.ad.onScreen = false;
    // Фильм вернулся новым поколением: пока кадра нет, плеер шлёт «0 / жду» (09a2).
    world.memory.playerClock(0, "waiting");
    assert.deepEqual(world.selects, [], "секунда рекламы сброшена, «жду» — не позиция");
    world.memory.playerClock(20 * 60 * 1_000_000, "playing");
    assert.deepEqual(world.selects, [`quality:${idOf("quality", "720p")}`]);
    assert.ok(world.notes.some((note) => /remembered audio not applied: the episode is already under way/u.test(note)));
  });

  it("в рекламе ничего не применяется; реклама кончилась — применяется", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.ad.onScreen = true;
    world.memory.playerClock(1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, []);
    world.ad.onScreen = false;
    world.memory.playerClock(1_250_000, "playing");
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub B")}`]);
  });

  it("субтитры: «выключены» — подсказка сразу; дорожка — только когда придёт дорожка этой серии", async () => {
    const off = harness({ remembered: { subtitles: { kind: "off" } } });
    off.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(off.preferences, [{ kind: "off" }]);

    const track = harness({ remembered: { subtitles: { kind: "track", language: "en", label: "English" } } });
    track.memory.subtitleTrackOffered({ track: 1, language: "en", label: "English" });
    track.clock.now += 10 * 60_000;
    track.memory.pageDescribed(page("Dub A", "360p", "2"));
    await settle();
    assert.deepEqual(track.preferences, [], "дорожка 1 — из прошлой серии");
    track.memory.subtitleTrackOffered({ track: 2, language: "en", label: "English" });
    track.memory.subtitleTrackOffered({ track: 3, language: "en", label: "English" });
    assert.deepEqual(track.preferences, [{ kind: "page_track", track: 2 }], "один раз на серию");
  });
});

describe("инкогнито (решение 285)", () => {
  it("приватное окно: запомненное применяется, новый выбор не пишется, время касания не меняется", async () => {
    const world = harness({ remembered: { audio: "Dub B" }, writing: "private_window" });
    world.memory.playerClock(1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub B")}`]);
    world.memory.selectionStarted("quality", "viewer");
    world.memory.selectionFinished("quality", "viewer", "done", "720p");
    await settle();
    assert.equal(world.storage.state.writes, 0);
    assert.deepEqual(world.stored(), { audio: "Dub B", used_at: 7 });
  });

  it("выбор сеанса в инкогнито действует до конца сеанса: следующая серия не возвращает старое", async () => {
    const world = harness({ remembered: { audio: "Dub B" }, writing: "private_window" });
    world.memory.playerClock(30 * 60 * 1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p", "1"));
    world.memory.selectionStarted("audio", "viewer");
    world.memory.selectionFinished("audio", "viewer", "done", "Dub C");
    world.afterSettle();
    world.memory.pageDescribed(page("Dub A", "360p", "2"));
    await settle();
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub C")}`]);
    assert.equal(world.stored()?.audio, "Dub B");
  });

  it("приватность окна неизвестна — тоже не пишется", async () => {
    const world = harness({ writing: "privacy_unknown" });
    world.memory.pageDescribed(page("Dub A", "360p"));
    world.memory.pageDescribed(page("Dub B", "360p"));
    await settle();
    assert.equal(world.storage.state.writes, 0);
  });
});

describe("жизненный цикл", () => {
  it("после stop память молчит, после resume (страница вернулась) снова работает", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.memory.stop();
    world.memory.playerClock(1_000_000, "playing");
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, []);
    world.memory.resume();
    world.memory.pageDescribed(page("Dub A", "360p"));
    await settle();
    assert.deepEqual(world.selects, [`audio:${idOf("audio", "Dub B")}`]);
  });

  it("применение отмечает касание (сериал, который смотрят, не вытесняется)", async () => {
    const world = harness({ remembered: { audio: "Dub B" } });
    world.memory.pageDescribed(page("Dub B", "360p"));
    await settle();
    assert.equal(world.stored()?.used_at, 100_000);
  });
});
