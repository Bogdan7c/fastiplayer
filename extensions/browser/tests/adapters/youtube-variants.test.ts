// Адаптер вариантов YouTube (15a) с НАСТОЯЩИМИ руками: рука DOM (подделка дерева) и рука-функция (настоящий
// клиент изолированного мира + настоящий исполнитель MAIN-мира). Плеер YouTube — маленькая подделка элемента
// `.html5-video-player` с функциями в «документе окна».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PlayerHandActions } from "../../src/adapters/capabilities/player-hand/contract.ts";
import type { VariantPageView } from "../../src/adapters/capabilities/variants/contract.ts";
import { YOUTUBE_PLAYER_HAND } from "../../src/adapters/sites/youtube/player-hand.ts";
import { YOUTUBE_SITE } from "../../src/adapters/sites/youtube/identity.ts";
import { YouTubePageError, createYouTubeVariants } from "../../src/adapters/sites/youtube/variants.ts";
import { createAdapterHandClient } from "../../src/content/adapter-hand-client.ts";
import { createAdapterHandExecutor } from "../../src/page/adapter-hand.ts";
import { createFakeClock } from "../background/support.ts";
import { createFakeAdView, FakeNode } from "./fake-ad-dom.ts";

// ------------------------------------------------------------------ подделка плеера YouTube

interface FakePlayerOptions {
  /** Пустой плеер (`#movie_player` в Shorts): `video_id` нет, качеств нет. */
  readonly empty?: boolean;
  readonly id?: string;
  readonly live?: boolean;
  /** Имя сжатого поля с описанием дорожки (меняется между сборками YouTube). */
  readonly compressedField?: string;
  readonly audioIds?: readonly string[];
  readonly currentAudioId?: string;
  readonly failSetAudio?: boolean;
  readonly qualities?: readonly unknown[];
  readonly preference?: unknown;
}

function audioTrack(field: string, id: string) {
  return { [field]: { name: `Track ${id}`, id, isDefault: false }, other: 1 };
}

function fakeYouTubePlayer(options: FakePlayerOptions = {}) {
  const field = options.compressedField ?? "Tb";
  const audioIds = options.audioIds ?? ["en.4", "uk.3"];
  const tracks = audioIds.map((id) => audioTrack(field, id));
  const state = { preference: options.preference ?? "auto", audio: options.currentAudioId ?? audioIds[0], nextCalls: 0, qualityRanges: [] as string[][], setTracks: [] as unknown[] };
  const player = {
    id: options.id ?? "movie_player",
    getVideoData: () => (options.empty === true ? {} : { video_id: "vid_AAA", isLive: options.live === true }),
    getAvailableQualityData: () =>
      options.empty === true
        ? []
        : (options.qualities ?? [
            { quality: "hd1080", qualityLabel: "1080p", isPlayable: true },
            { quality: "large", qualityLabel: "480p", isPlayable: false },
            { quality: "small", qualityLabel: "240p", isPlayable: true },
          ]),
    getUserPlaybackQualityPreference: () => state.preference,
    setPlaybackQualityRange(low: string, high: string) {
      state.qualityRanges.push([low, high]);
      state.preference = low;
    },
    getAvailableAudioTracks: () => (options.empty === true || options.live === true ? [] : tracks),
    getAudioTrack: () => tracks.find((track) => (Reflect.get(track, field) as { id: string }).id === state.audio) ?? null,
    setAudioTrack(track: unknown) {
      if (options.failSetAudio === true) throw new Error("setAudioTrack crashed");
      state.setTracks.push(track);
      state.audio = (Reflect.get(track as object, field) as { id: string }).id;
      return true;
    },
    nextVideo() {
      state.nextCalls += 1;
    },
  };
  return { player, tracks, state };
}

type FakePlayer = ReturnType<typeof fakeYouTubePlayer>["player"];

/** «Окно» страницы: документ отдаёт плееры в порядке разметки. */
function fakeWindow(players: readonly object[], pathname = "/watch"): object {
  return { document: { querySelectorAll: (selector: string) => (selector === ".html5-video-player" ? players : []) }, location: { pathname } };
}

// ------------------------------------------------------------------ подделка DOM и руки

/** Страница ролика: плеер, а в нём кнопка «следующее» с адресом/без. */
function watchDom(next: { href?: string; disabled?: boolean; rendered?: boolean } | "none") {
  const root = new FakeNode("html");
  const playerNode = new FakeNode("div", { id: "movie_player", classes: ["html5-video-player"] });
  root.add(playerNode);
  if (next !== "none") {
    const attributes: Record<string, string> = {};
    if (next.href !== undefined) attributes.href = next.href;
    if (next.disabled === true) attributes["aria-disabled"] = "true";
    playerNode.add(new FakeNode("a", { classes: ["ytp-next-button"], attributes, rendered: next.rendered ?? true }));
  }
  return root;
}

function variantView(root: FakeNode, hostname = "www.youtube.com"): VariantPageView {
  return { ...createFakeAdView(root, { hostname }), attributeOf: (element, name) => (element as FakeNode).attributes.get(name) ?? null };
}

function adapterFor(root: FakeNode, page: object, hostname = "www.youtube.com", actions: PlayerHandActions = YOUTUBE_PLAYER_HAND) {
  const clock = createFakeClock();
  let client: ReturnType<typeof createAdapterHandClient> | undefined;
  const executor = createAdapterHandExecutor({ page, actionsOf: (siteId) => (siteId === YOUTUBE_SITE.id ? actions : undefined), send: (reply) => queueMicrotask(() => client?.acceptPageMessage(structuredClone(reply))) });
  client = createAdapterHandClient({
    sendToPage: (call) => queueMicrotask(() => void executor.handle(structuredClone(call))),
    timers: clock.scheduler,
    now: clock.scheduler.now,
  });
  return createYouTubeVariants({ dom: variantView(root, hostname), player: client.handFor(YOUTUBE_SITE.id) });
}

const WITH_NEXT = { href: "/watch?v=next_one" } as const;

// ------------------------------------------------------------------ тесты

describe("адаптер вариантов YouTube: опознание", () => {
  it("свой на youtube.com и youtube-nocookie.com (встроенный) с плеером в документе; чужой хост и страница без плеера — нет", () => {
    const root = watchDom("none");
    const window = fakeWindow([]);
    assert.equal(adapterFor(root, window, "www.youtube.com").matches(variantView(root, "www.youtube.com")), true);
    assert.equal(adapterFor(root, window).matches(variantView(root, "www.youtube-nocookie.com")), true);
    assert.equal(adapterFor(root, window).matches(variantView(root, "www.example.com")), false);
    assert.equal(adapterFor(root, window).matches(variantView(root, "notyoutube.com")), false);
    assert.equal(adapterFor(root, window).matches(variantView(new FakeNode("html"))), false);
  });

  it("следит за всем документом: в Shorts первым стоит пустой плеер", () => {
    const root = watchDom(WITH_NEXT);
    assert.equal(adapterFor(root, fakeWindow([])).observationRoot(), root);
  });
});

describe("адаптер вариантов YouTube: описание через обе руки", () => {
  it("ролик: ключи, меню качеств (Auto первым, непроигрываемые пропущены), озвучки по коду языка, следующий ролик", async () => {
    const { player } = fakeYouTubePlayer();
    const description = await adapterFor(watchDom(WITH_NEXT), fakeWindow([player])).describe();
    assert.deepEqual(description, {
      seriesKey: "youtube",
      episodeKey: "vid_AAA",
      audio: [
        { id: "en", label: "Track en.4", selected: true },
        { id: "uk", label: "Track uk.3", selected: false },
      ],
      quality: [
        { id: "auto", label: "Auto", selected: true, automatic: true },
        { id: "hd1080", label: "1080p", selected: false },
        { id: "small", label: "240p", selected: false },
      ],
      nextEpisode: true,
    });
  });

  it("уровней нет (встроенный плеер до старта, все непроигрываемые) — меню качеств пустое, одинокого Auto нет", async () => {
    const { player } = fakeYouTubePlayer({ qualities: [{ quality: "hd720", qualityLabel: "720p", isPlayable: false }] });
    const description = await adapterFor(watchDom(WITH_NEXT), fakeWindow([player])).describe();
    assert.deepEqual(description?.quality, []);
    assert.equal(description?.audio.length, 2, "озвучки от этого не зависят");
  });

  it("подпись качества — ступень: «720p60» и «1080p Premium» сайта становятся «720p» и «1080p» (память по подписи, 15a)", async () => {
    const { player } = fakeYouTubePlayer({
      qualities: [
        { quality: "hd1080", qualityLabel: "1080p Premium", isPlayable: true },
        { quality: "hd720", qualityLabel: "720p60", isPlayable: true },
      ],
    });
    const description = await adapterFor(watchDom(WITH_NEXT), fakeWindow([player])).describe();
    assert.deepEqual(
      description?.quality.map((option) => [option.id, option.label]),
      [
        ["auto", "Auto"],
        ["hd1080", "1080p"],
        ["hd720", "720p"],
      ],
    );
  });

  it("выбор человека — уровень: selected у него, а не у Auto", async () => {
    const { player } = fakeYouTubePlayer({ preference: "small" });
    const description = await adapterFor(watchDom(WITH_NEXT), fakeWindow([player])).describe();
    assert.deepEqual(description?.quality.map((option) => option.selected), [false, false, true]);
  });

  it("сжатое поле под другим именем (Xy) находится по форме", async () => {
    const { player } = fakeYouTubePlayer({ compressedField: "Xy", audioIds: ["zh-Hant.3", "en.4"], currentAudioId: "zh-Hant.3" });
    const description = await adapterFor(watchDom("none"), fakeWindow([player])).describe();
    assert.deepEqual(description?.audio, [
      { id: "zh-Hant", label: "Track zh-Hant.3", selected: true },
      { id: "en", label: "Track en.4", selected: false },
    ]);
  });

  it("дорожка без вложенного описания, язык вне алфавита и повтор языка пропускаются; первый повтор остаётся", async () => {
    const { player, tracks } = fakeYouTubePlayer({ audioIds: ["en.4", "en.9", "ru ё.1", "de.2"] });
    // Дорожка без вложенного описания: только плоские поля.
    const withoutDescriptor = { name: "flat", id: "fr.1" };
    const patched = { ...player, getAvailableAudioTracks: () => [withoutDescriptor, ...tracks] };
    const description = await adapterFor(watchDom("none"), fakeWindow([patched])).describe();
    assert.deepEqual(description?.audio.map((option) => [option.id, option.label]), [
      ["en", "Track en.4"],
      ["de", "Track de.2"],
    ]);
  });

  it("эфир: качества есть, озвучек нет; кнопка «следующее» без адреса или выключенная — nextEpisode false", async () => {
    const { player } = fakeYouTubePlayer({ live: true });
    const noHref = await adapterFor(watchDom({}), fakeWindow([player])).describe();
    assert.deepEqual(noHref?.audio, []);
    assert.equal(noHref?.quality.length, 3);
    assert.equal(noHref?.nextEpisode, false);
    assert.equal((await adapterFor(watchDom({ href: "/x", disabled: true }), fakeWindow([player])).describe())?.nextEpisode, false);
    assert.equal((await adapterFor(watchDom("none"), fakeWindow([player])).describe())?.nextEpisode, false);
    // Скрытая (display:none) кнопка с адресом — следующий ролик есть.
    assert.equal((await adapterFor(watchDom({ href: "/x", rendered: false }), fakeWindow([player])).describe())?.nextEpisode, true);
  });

  it("Shorts с двумя плеерами (пустой movie_player первым): берётся активный, nextEpisode false", async () => {
    const empty = fakeYouTubePlayer({ empty: true }).player;
    const shorts = fakeYouTubePlayer({ id: "shorts-player", audioIds: [] }).player;
    const description = await adapterFor(watchDom(WITH_NEXT), fakeWindow([empty, shorts], "/shorts/abc")).describe();
    assert.equal(description?.episodeKey, "vid_AAA");
    assert.equal(description?.nextEpisode, false);
    assert.equal(description?.quality.length, 3);
  });

  it("активного плеера нет (все пустые) или нет документа — честное «сейчас ничего» / срыв", async () => {
    const empty = fakeYouTubePlayer({ empty: true }).player;
    assert.equal(await adapterFor(watchDom(WITH_NEXT), fakeWindow([empty])).describe(), null);
    assert.equal(await adapterFor(watchDom(WITH_NEXT), fakeWindow([])).describe(), null);
    // Окно без документа: плеера тоже нет — «ничего», а не срыв и не выдумка.
    assert.equal(await adapterFor(watchDom(WITH_NEXT), {}).describe(), null);
    // Рука не знает действия (белый список другой версии) — это срыв.
    await assert.rejects(adapterFor(watchDom(WITH_NEXT), fakeWindow([]), "www.youtube.com", {}).describe(), /describe_player: no_such_action/u);
  });

  it("мусор в ответах плеера: качество не объект, поля не тех типов — описание срывается; бросающая функция — тоже", async () => {
    const base = fakeYouTubePlayer().player;
    const build = (patch: Partial<Record<keyof FakePlayer, unknown>>) => fakeWindow([{ ...base, ...patch }]);
    // Мусорные записи качеств отсекаются на стороне MAIN (нет строк) и описание остаётся целым…
    const tolerated = await adapterFor(watchDom("none"), build({ getAvailableQualityData: () => [null, 5, { quality: 1 }, { quality: "hd720", qualityLabel: "720p" }] })).describe();
    assert.deepEqual(tolerated?.quality.map((option) => option.id), ["auto", "hd720"]);
    // …а бросающая функция плеера обрывает описание целиком.
    await assert.rejects(adapterFor(watchDom("none"), build({ getAvailableQualityData: () => { throw new Error("boom"); } })).describe(), /describe_player: failed/u);
  });
});

describe("адаптер вариантов YouTube: строгая форма ответа руки", () => {
  const validPlayer = { videoId: "v", live: false, shorts: false, qualities: [], qualityPreference: "auto", audio: [] };
  const describeWith = (answer: unknown) =>
    adapterFor(watchDom("none"), fakeWindow([]), "www.youtube.com", { describe_player: () => answer }).describe();

  it("ответ нужной формы принимается; не та форма любого поля — YouTubePageError, описания нет", async () => {
    assert.equal((await describeWith(validPlayer))?.episodeKey, "v");
    const broken: unknown[] = [
      "text",
      [],
      { ...validPlayer, videoId: "" },
      { ...validPlayer, live: "no" },
      { ...validPlayer, qualityPreference: 5 },
      { ...validPlayer, qualities: [{ level: "hd720" }] },
      { ...validPlayer, qualities: "none" },
      { ...validPlayer, audio: [{ language: "en", label: "x", selected: "yes" }] },
    ];
    for (const answer of broken) await assert.rejects(describeWith(answer), YouTubePageError);
  });

  it("мусорный id не доходит даже до руки-функции (проверка до MAIN-мира)", async () => {
    const reached: string[] = [];
    const adapter = adapterFor(watchDom(WITH_NEXT), fakeWindow([]), "www.youtube.com", {
      select_quality: (_page, argument) => (reached.push(String(argument)), true),
      select_audio: (_page, argument) => (reached.push(String(argument)), true),
    });
    assert.equal(await adapter.select("quality", "a b"), "no_such_variant");
    assert.equal(await adapter.select("audio", "ё"), "no_such_variant");
    assert.equal(await adapter.select("audio", "uk"), "done");
    assert.deepEqual(reached, ["uk"]);
  });

  it("действие, ответившее не true/false, — failed", async () => {
    const adapter = adapterFor(watchDom(WITH_NEXT), fakeWindow([]), "www.youtube.com", { select_quality: () => "ok", next_video: () => 1 });
    assert.equal(await adapter.select("quality", "hd720"), "failed");
    assert.equal(await adapter.next(), "failed");
  });
});

describe("адаптер вариантов YouTube: действия", () => {
  it("качество: уровень → setPlaybackQualityRange(уровень, уровень); Auto → (auto, auto); нет такого и непроигрываемый — no_such_variant", async () => {
    const fake = fakeYouTubePlayer();
    const adapter = adapterFor(watchDom(WITH_NEXT), fakeWindow([fake.player]));
    assert.equal(await adapter.select("quality", "small"), "done");
    assert.equal(await adapter.select("quality", "auto"), "done");
    assert.deepEqual(fake.state.qualityRanges, [["small", "small"], ["auto", "auto"]]);
    assert.equal(await adapter.select("quality", "hd4320"), "no_such_variant");
    assert.equal(await adapter.select("quality", "large"), "no_such_variant", "непроигрываемый уровень выбрать нельзя");
    assert.equal(fake.state.qualityRanges.length, 2);
  });

  it("озвучка: страница получает ту же дорожку из своего списка; поток переключился; нет языка — no_such_variant", async () => {
    const fake = fakeYouTubePlayer();
    const adapter = adapterFor(watchDom(WITH_NEXT), fakeWindow([fake.player]));
    assert.equal(await adapter.select("audio", "uk"), "done");
    assert.equal(fake.state.setTracks[0], fake.tracks[1]);
    assert.equal(fake.state.audio, "uk.3");
    assert.equal(await adapter.select("audio", "fr"), "no_such_variant");
    assert.equal(fake.state.setTracks.length, 1);
  });

  it("setAudioTrack бросил — failed; setAudioTrack вернул false — no_such_variant", async () => {
    const throwing = fakeYouTubePlayer({ failSetAudio: true });
    assert.equal(await adapterFor(watchDom(WITH_NEXT), fakeWindow([throwing.player])).select("audio", "uk"), "failed");
    const refusing = fakeYouTubePlayer();
    const window = fakeWindow([{ ...refusing.player, setAudioTrack: () => false }]);
    assert.equal(await adapterFor(watchDom(WITH_NEXT), window).select("audio", "uk"), "no_such_variant");
  });

  it("id с мусором ничего не вызывает у страницы", async () => {
    const fake = fakeYouTubePlayer();
    const adapter = adapterFor(watchDom(WITH_NEXT), fakeWindow([fake.player]));
    for (const garbage of ["", "a b", "uk'); alert(1);//", "ё", "x".repeat(65)]) {
      assert.equal(await adapter.select("audio", garbage), "no_such_variant");
      assert.equal(await adapter.select("quality", garbage), "no_such_variant");
    }
    assert.deepEqual([fake.state.setTracks.length, fake.state.qualityRanges.length, fake.state.nextCalls], [0, 0, 0]);
  });

  it("выбор при активном плеере в Shorts идёт в активный, а не в пустой movie_player", async () => {
    const empty = fakeYouTubePlayer({ empty: true });
    const shorts = fakeYouTubePlayer({ id: "shorts-player" });
    const adapter = adapterFor(watchDom(WITH_NEXT), fakeWindow([empty.player, shorts.player], "/shorts/x"));
    assert.equal(await adapter.select("quality", "small"), "done");
    assert.deepEqual(shorts.state.qualityRanges, [["small", "small"]]);
    assert.deepEqual(empty.state.qualityRanges, []);
  });

  it("следующий ролик: кнопка с адресом — nextVideo вызван; без кнопки — no_such_variant; Shorts — no_such_variant; плеер без документа — failed", async () => {
    const fake = fakeYouTubePlayer();
    assert.equal(await adapterFor(watchDom(WITH_NEXT), fakeWindow([fake.player])).next(), "done");
    assert.equal(fake.state.nextCalls, 1);
    assert.equal(await adapterFor(watchDom("none"), fakeWindow([fake.player])).next(), "no_such_variant");
    assert.equal(fake.state.nextCalls, 1);
    const shorts = fakeYouTubePlayer({ id: "shorts-player" });
    assert.equal(await adapterFor(watchDom(WITH_NEXT), fakeWindow([shorts.player], "/shorts/x")).next(), "no_such_variant");
    assert.equal(shorts.state.nextCalls, 0);
    assert.equal(await adapterFor(watchDom(WITH_NEXT), {}).next(), "failed");
  });

  it("плеер без функции nextVideo — failed, а не «сделано»", async () => {
    const { player } = fakeYouTubePlayer();
    const bare = { ...player, nextVideo: undefined };
    assert.equal(await adapterFor(watchDom(WITH_NEXT), fakeWindow([bare])).next(), "failed");
  });
});
