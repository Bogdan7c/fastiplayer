// Меню субтитров YouTube (15a2) с НАСТОЯЩИМИ руками: адаптер изолированного мира, настоящий клиент руки и
// настоящий исполнитель MAIN-мира; плеер YouTube - подделка с `getPlayerResponse`, `getOption`, `setOption`.
// Главное, что проверяем: ЧТО получил `setOption` подделки (то есть что бы сделал настоящий YouTube).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { englishLanguageName } from "../../src/adapters/capabilities/subtitle-naming/caption-label.ts";
import type { PlayerHandActions } from "../../src/adapters/capabilities/player-hand/contract.ts";
import type { PageVariantOption, VariantPageView } from "../../src/adapters/capabilities/variants/contract.ts";
import { YOUTUBE_PLAYER_HAND } from "../../src/adapters/sites/youtube/player-hand.ts";
import { YOUTUBE_SITE } from "../../src/adapters/sites/youtube/identity.ts";
import { createYouTubeVariants } from "../../src/adapters/sites/youtube/variants.ts";
import { createAdapterHandClient } from "../../src/content/adapter-hand-client.ts";
import { createAdapterHandExecutor } from "../../src/page/adapter-hand.ts";
import { createFakeClock } from "../background/support.ts";
import { createFakeVariantView, FakeNode } from "./fake-ad-dom.ts";

// ------------------------------------------------------------------ подделка плеера с субтитрами

interface RawTrack {
  readonly languageCode: string;
  readonly vssId: string;
  readonly kind?: string;
  readonly name?: unknown;
  readonly isTranslatable?: boolean;
}

interface CaptionsPlayerOptions {
  readonly tracks?: readonly unknown[];
  readonly translations?: readonly string[];
  /** Что отвечает `getOption("captions","track")`: объект, исключение (`"throw"`) или по умолчанию — пустой объект. */
  readonly current?: unknown;
  /** Без `getPlayerResponse` вовсе (страница не та). */
  readonly noPlayerResponse?: boolean;
  readonly noSetOption?: boolean;
  /** Весь `getPlayerResponse()` целиком (мусор). */
  readonly response?: unknown;
}

const track = (languageCode: string, vssId: string, extra: Partial<RawTrack> = {}): RawTrack => ({
  languageCode,
  vssId,
  name: { simpleText: languageCode },
  isTranslatable: true,
  ...extra,
});

function captionsPlayer(options: CaptionsPlayerOptions = {}) {
  const setOptionCalls: { readonly module: unknown; readonly option: unknown; readonly value: unknown }[] = [];
  const state = { tracks: [...(options.tracks ?? [])], current: options.current ?? {} };
  const player: Record<string, unknown> = {
    id: "movie_player",
    getVideoData: () => ({ video_id: "vid_AAA", isLive: false }),
    getAvailableQualityData: () => [],
    getUserPlaybackQualityPreference: () => "auto",
    getAvailableAudioTracks: () => [],
    getAudioTrack: () => null,
    getOption: (_module: string, _option: string) => {
      if (state.current === "throw") throw new Error("captions module is not loaded");
      return state.current;
    },
  };
  if (options.noPlayerResponse !== true) {
    player.getPlayerResponse = () =>
      "response" in options
        ? options.response
        : { captions: { playerCaptionsTracklistRenderer: { captionTracks: state.tracks, translationLanguages: (options.translations ?? []).map((code) => ({ languageCode: code })) } } };
  }
  if (options.noSetOption !== true) {
    player.setOption = (module: unknown, option: unknown, value: unknown) => {
      setOptionCalls.push({ module, option, value });
    };
  }
  return { player, state, setOptionCalls };
}

function fakeWindow(players: readonly object[]): object {
  return { document: { querySelectorAll: (selector: string) => (selector === ".html5-video-player" ? players : []) }, location: { pathname: "/watch" } };
}

// ------------------------------------------------------------------ проводка рук (как в youtube-variants.test.ts)

function watchDom(): FakeNode {
  const root = new FakeNode("html");
  root.add(new FakeNode("div", { id: "movie_player", classes: ["html5-video-player"] }));
  return root;
}

function adapterFor(page: object, actions: PlayerHandActions = YOUTUBE_PLAYER_HAND) {
  const root = watchDom();
  const clock = createFakeClock();
  let client: ReturnType<typeof createAdapterHandClient> | undefined;
  const executor = createAdapterHandExecutor({ page, actionsOf: (siteId) => (siteId === YOUTUBE_SITE.id ? actions : undefined), send: (reply) => queueMicrotask(() => client?.acceptPageMessage(structuredClone(reply))) });
  client = createAdapterHandClient({
    sendToPage: (call) => queueMicrotask(() => void executor.handle(structuredClone(call))),
    timers: clock.scheduler,
    now: clock.scheduler.now,
  });
  const view: VariantPageView = createFakeVariantView(root, { hostname: "www.youtube.com" });
  return createYouTubeVariants({ dom: view, player: client.handFor(YOUTUBE_SITE.id) });
}

/** Меню субтитров плеера в виде «подпись:выбран». */
async function subtitleLabels(fake: ReturnType<typeof captionsPlayer>): Promise<string[]> {
  const description = await adapterFor(fakeWindow([fake.player])).describe();
  return (description?.subtitles ?? []).map((option) => `${option.label}${option.selected ? "*" : ""}`);
}

/** Id пункта с этой подписью (без отметки выбора). */
async function idOfLabel(fake: ReturnType<typeof captionsPlayer>, label: string): Promise<string> {
  const description = await adapterFor(fakeWindow([fake.player])).describe();
  const option = description?.subtitles.find((candidate) => candidate.label === label);
  assert.ok(option, `в меню нет пункта «${label}»`);
  return option.id;
}

const SAMPLE_TRACKS = [
  track("en", ".en", { name: { simpleText: "English" } }),
  track("en", "a.en", { kind: "asr", name: { runs: [{ text: "English" }, { text: " (auto-generated)" }] } }),
  track("uk", ".uk", { name: { simpleText: "Ukrainian" }, isTranslatable: false }),
];

// ------------------------------------------------------------------ тесты

describe("меню субтитров YouTube: описание через обе руки", () => {
  it("сначала родные дорожки в порядке YouTube, потом автоперевод; подписи английские; язык без английского названия подписан кодом, мусорный код пропущен", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de", "uk", "zh-Hans", "zz", "bad code"] });
    assert.deepEqual(await subtitleLabels(fake), [
      "English",
      "English (auto-generated)",
      "Ukrainian",
      "German (auto-translated)",
      "Ukrainian (auto-translated)",
      "Simplified Chinese (auto-translated)",
      "zz (auto-translated)",
    ]);
  });

  it("id пунктов проходят правило протокола; родной id ведёт к дорожке, id перевода называет язык", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de", "zh-Hans"] });
    const description = await adapterFor(fakeWindow([fake.player])).describe();
    const ids = description?.subtitles.map((option) => option.id) ?? [];
    assert.equal(ids.length, 5);
    for (const id of ids) assert.match(id, /^[A-Za-z0-9_-]{1,64}$/u);
    assert.deepEqual(ids.slice(3), ["x-de", "x-zh-Hans"]);
    assert.equal(new Set(ids).size, ids.length);
  });

  it("выбрана родная дорожка (в том числе автосубтитры) — отмечена она одна", async () => {
    const asr = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de"], current: { languageCode: "en", vss_id: "a.en", kind: "asr" } });
    assert.deepEqual(await subtitleLabels(asr), ["English", "English (auto-generated)*", "Ukrainian", "German (auto-translated)"]);
  });

  it("выбран перевод — отмечен только перевод, а исходная дорожка нет", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de", "fr"], current: { languageCode: "en", vss_id: ".en", translationLanguage: { languageCode: "de" } } });
    assert.deepEqual(await subtitleLabels(fake), ["English", "English (auto-generated)", "Ukrainian", "German (auto-translated)*", "French (auto-translated)"]);
  });

  it("выбора нет: пустой объект, исключение плеера и vss_id вне списка — ничего не отмечено, меню целое", async () => {
    for (const current of [{}, "throw", { vss_id: "gone.xx" }, null]) {
      const labels = await subtitleLabels(captionsPlayer({ tracks: SAMPLE_TRACKS, current }));
      assert.deepEqual(labels, ["English", "English (auto-generated)", "Ukrainian"], `current=${String(current)}`);
    }
  });

  it("две родные дорожки одного языка различаются названием YouTube в скобках; одиночные подписи не меняются", async () => {
    const fake = captionsPlayer({
      tracks: [
        track("en", ".en.a", { name: { simpleText: "British" } }),
        track("en", ".en.b", { name: { runs: [{ text: "Ame" }, { text: "rican" }] } }),
        track("de", ".de", { name: { simpleText: "Deutsch" } }),
      ],
    });
    assert.deepEqual(await subtitleLabels(fake), ["English (British)", "English (American)", "German"]);
  });

  it("язык без английского названия: подпись берётся у YouTube, а без неё - код языка", async () => {
    const fake = captionsPlayer({ tracks: [track("zz", ".zz", { name: { simpleText: "Zed" } }), track("qq", ".qq", { name: undefined })] });
    assert.deepEqual(await subtitleLabels(fake), ["Zed", "qq"]);
  });

  it("переводить нечего (ни одной переводимой дорожки) — переводов в меню нет", async () => {
    const fake = captionsPlayer({ tracks: [track("uk", ".uk", { isTranslatable: false })], translations: ["de", "fr"] });
    assert.deepEqual(await subtitleLabels(fake), ["Ukrainian"]);
  });

  it("ролик без субтитров, эфир без ответа, плеер без getPlayerResponse - меню пустое, озвучки и качества живы", async () => {
    const empty = captionsPlayer({ tracks: [], translations: ["de"] });
    assert.deepEqual(await subtitleLabels(empty), []);
    const noCaptionsKey = captionsPlayer({ response: { videoDetails: {} } });
    assert.deepEqual(await subtitleLabels(noCaptionsKey), []);
    const missing = captionsPlayer({ noPlayerResponse: true });
    const description = await adapterFor(fakeWindow([missing.player])).describe();
    assert.deepEqual(description?.subtitles, []);
    assert.equal(description?.episodeKey, "vid_AAA");
  });

  it("мусор от страницы: дорожки без языка или vssId пропускаются, ответ не-объект и длинные строки не роняют описание", async () => {
    const fake = captionsPlayer({
      tracks: [null, 5, { languageCode: "", vssId: ".x" }, { languageCode: "en" }, track("en", ".en", { name: { simpleText: "E".repeat(5_000) } })],
      translations: [],
    });
    assert.deepEqual(await subtitleLabels(fake), ["English"]);
    assert.deepEqual(await subtitleLabels(captionsPlayer({ response: "junk" })), []);
    assert.deepEqual(await subtitleLabels(captionsPlayer({ response: { captions: { playerCaptionsTracklistRenderer: { captionTracks: "no", translationLanguages: 7 } } } })), []);
  });

  it("мусор в ответе РУКИ (поле captions не той формы) даёт пустое меню субтитров и не отнимает озвучки и качества", async () => {
    const valid = { videoId: "v", live: false, shorts: false, qualities: [{ level: "hd720", label: "720p" }], qualityPreference: "auto", audio: [{ language: "en", label: "English", selected: true }] };
    const garbageCaptions: unknown[] = [
      "junk",
      null,
      { tracks: "no", translations: [], current: null },
      { tracks: [{ id: "t0_a" }], translations: [], current: null },
      { tracks: [], translations: [5], current: null },
      { tracks: [], translations: [] },
      { tracks: [], translations: [], current: { vssId: 5, translation: null } },
    ];
    for (const captions of garbageCaptions) {
      const description = await adapterFor({}, { describe_player: () => ({ ...valid, captions }) }).describe();
      assert.deepEqual(description?.subtitles, [], JSON.stringify(captions));
      assert.deepEqual(description?.audio.map((option) => option.id), ["en"]);
      assert.deepEqual(description?.quality.map((option) => option.id), ["auto", "hd720"]);
    }
    // Рука старой версии вовсе без поля - то же самое.
    const without = await adapterFor({}, { describe_player: () => valid }).describe();
    assert.deepEqual(without?.subtitles, []);
    assert.equal(without?.audio.length, 1);
  });

  it("лимит 256 пунктов: родные дорожки целы, режутся переводы; описание проходит пределы руки", async () => {
    const natives = Array.from({ length: 100 }, (_unused, index) => track("en", `.en.${index}`, { name: { simpleText: `Variant ${index}` } }));
    const codes: string[] = [];
    for (const first of "abcdefghijklmnopqrstuvwxyz") {
      for (const second of "abcdefghijklmnopqrstuvwxyz") if (englishLanguageName(`${first}${second}`) !== undefined) codes.push(`${first}${second}`);
    }
    assert.ok(codes.length >= 160, "для проверки нужно много языков с названиями");
    const fake = captionsPlayer({ tracks: natives, translations: codes });
    const description = await adapterFor(fakeWindow([fake.player])).describe();
    assert.ok(description, "описание не сорвано пределами руки");
    assert.equal(description.subtitles.length, 256);
    assert.deepEqual(description.subtitles.slice(0, 100).map((option) => option.label), natives.map((_native, index) => `English (Variant ${index})`));
    assert.ok(description.subtitles.slice(100).every((option) => option.id.startsWith("x-")));
  });
});

describe("меню субтитров YouTube: выбор доходит до setOption плеера", () => {
  it("родная дорожка: setOption(captions, track, {languageCode, vss_id}); у автосубтитров ещё и kind", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de"] });
    const adapter = adapterFor(fakeWindow([fake.player]));
    assert.equal(await adapter.select("subtitles", await idOfLabel(fake, "Ukrainian")), "done");
    assert.equal(await adapter.select("subtitles", await idOfLabel(fake, "English (auto-generated)")), "done");
    assert.equal(await adapter.select("subtitles", await idOfLabel(fake, "English")), "done");
    assert.deepEqual(fake.setOptionCalls, [
      { module: "captions", option: "track", value: { languageCode: "uk", vss_id: ".uk" } },
      { module: "captions", option: "track", value: { languageCode: "en", vss_id: "a.en", kind: "asr" } },
      { module: "captions", option: "track", value: { languageCode: "en", vss_id: ".en" } },
    ]);
  });

  it("перевод идёт с дорожки, выбранной на странице, если её можно переводить", async () => {
    const tracks = [track("en", ".en"), track("de", ".de"), track("uk", ".uk", { isTranslatable: false })];
    const fake = captionsPlayer({ tracks, translations: ["fr"], current: { languageCode: "de", vss_id: ".de" } });
    assert.equal(await adapterFor(fakeWindow([fake.player])).select("subtitles", "x-fr"), "done");
    assert.deepEqual(fake.setOptionCalls.map((call) => call.value), [{ languageCode: "de", vss_id: ".de", translationLanguage: { languageCode: "fr" } }]);
  });

  it("выбранная дорожка не переводится (или выбора нет) — источник первая РУЧНАЯ переводимая, а не автосубтитры раньше неё", async () => {
    const tracks = [track("en", "a.en", { kind: "asr" }), track("uk", ".uk", { isTranslatable: false }), track("de", ".de"), track("fr", ".fr")];
    const noneSelected = captionsPlayer({ tracks, translations: ["pl"] });
    assert.equal(await adapterFor(fakeWindow([noneSelected.player])).select("subtitles", "x-pl"), "done");
    assert.deepEqual(noneSelected.setOptionCalls.map((call) => call.value), [{ languageCode: "de", vss_id: ".de", translationLanguage: { languageCode: "pl" } }]);
    const untranslatableSelected = captionsPlayer({ tracks, translations: ["pl"], current: { languageCode: "uk", vss_id: ".uk" } });
    assert.equal(await adapterFor(fakeWindow([untranslatableSelected.player])).select("subtitles", "x-pl"), "done");
    assert.equal((untranslatableSelected.setOptionCalls[0]?.value as { vss_id: string }).vss_id, ".de");
  });

  it("переводимы только автосубтитры — источник они, и kind=asr уходит вместе с ними", async () => {
    const tracks = [track("uk", ".uk", { isTranslatable: false }), track("en", "a.en", { kind: "asr" })];
    const fake = captionsPlayer({ tracks, translations: ["de"] });
    assert.equal(await adapterFor(fakeWindow([fake.player])).select("subtitles", "x-de"), "done");
    assert.deepEqual(fake.setOptionCalls.map((call) => call.value), [{ languageCode: "en", vss_id: "a.en", kind: "asr", translationLanguage: { languageCode: "de" } }]);
  });

  it("неизвестный и негодный id ничего не вызывает: чужой номер, чужой язык, нет переводимых, мусор", async () => {
    const withTracks = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de"] });
    const nothingToTranslate = captionsPlayer({ tracks: [track("uk", ".uk", { isTranslatable: false })], translations: ["de"] });
    const outcomes: string[] = [];
    for (const id of ["t9_zzz", "t0_wrong", "t0", "x-qq", "x-", "", "a b", "t0_x'); alert(1);//", "ё", "x".repeat(65)]) {
      outcomes.push(await adapterFor(fakeWindow([withTracks.player])).select("subtitles", id));
    }
    outcomes.push(await adapterFor(fakeWindow([nothingToTranslate.player])).select("subtitles", "x-de"));
    assert.ok(outcomes.every((outcome) => outcome === "no_such_variant"), outcomes.join());
    assert.deepEqual([withTracks.setOptionCalls.length, nothingToTranslate.setOptionCalls.length], [0, 0]);
  });

  it("гонка: список пересобран между описанием и выбором - старый id не попадает на соседа, setOption не зовётся", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS });
    const staleId = await idOfLabel(fake, "Ukrainian");
    // YouTube пересобрал список: на месте «Ukrainian» теперь другая дорожка (смена ролика).
    fake.state.tracks = [track("fr", ".fr"), track("de", ".de"), track("pl", ".pl")];
    assert.equal(await adapterFor(fakeWindow([fake.player])).select("subtitles", staleId), "no_such_variant");
    assert.equal(fake.setOptionCalls.length, 0);
    // А свежее описание даёт id, который работает.
    assert.equal(await adapterFor(fakeWindow([fake.player])).select("subtitles", await idOfLabel(fake, "Polish")), "done");
    assert.equal((fake.setOptionCalls[0]?.value as { vss_id: string }).vss_id, ".pl");
  });

  it("у плеера нет setOption или getPlayerResponse - failed (страница не та), а не «сделано»", async () => {
    const noSetOption = captionsPlayer({ tracks: SAMPLE_TRACKS, noSetOption: true });
    assert.equal(await adapterFor(fakeWindow([noSetOption.player])).select("subtitles", await idOfLabel(noSetOption, "Ukrainian")), "failed");
    const noResponse = captionsPlayer({ noPlayerResponse: true });
    assert.equal(await adapterFor(fakeWindow([noResponse.player])).select("subtitles", "t0_abc"), "failed");
  });

  it("setOption бросил - failed", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS });
    fake.player.setOption = () => {
      throw new Error("player crashed");
    };
    assert.equal(await adapterFor(fakeWindow([fake.player])).select("subtitles", await idOfLabel(fake, "Ukrainian")), "failed");
  });
});

describe("меню субтитров YouTube: опции типа", () => {
  it("PageVariantOption без automatic: пункты субтитров - обычные, не «Auto» сайта", async () => {
    const fake = captionsPlayer({ tracks: SAMPLE_TRACKS, translations: ["de"] });
    const description = await adapterFor(fakeWindow([fake.player])).describe();
    const options: readonly PageVariantOption[] = description?.subtitles ?? [];
    assert.ok(options.length > 0 && options.every((option) => option.automatic === undefined));
  });
});
