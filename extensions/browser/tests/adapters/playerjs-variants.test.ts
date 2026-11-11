// Адаптер вариантов PlayerJS (15b) по фикстурам разведки — с НАСТОЯЩИМИ обеими руками: «нажатие как человек»
// по подделке DOM и рука-функция (настоящий клиент изолированного мира + настоящий исполнитель MAIN-мира,
// соединённые подделкой личного порта). Плеер страницы — подделка `pljssglobal[0].api`, которая отвечает
// на нажатия и команды так, как настоящие Ashdi и Tortuga (`fake-playerjs-page.ts`).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageVariantDescription } from "../../src/adapters/capabilities/variants/contract.ts";
import { playerHandActionsOf } from "../../src/adapters/registry/page-world.ts";
import type { PlayerJsPause } from "../../src/adapters/sites/playerjs/folder-dub-switch.ts";
import { PLAYERJS_SITE } from "../../src/adapters/sites/playerjs/identity.ts";
import { createPlayerJsVariants, PlayerJsPageError } from "../../src/adapters/sites/playerjs/variants.ts";
import { createAdapterHandClient, type AdapterHandClient } from "../../src/content/adapter-hand-client.ts";
import { createAdapterHandExecutor } from "../../src/page/adapter-hand.ts";
import { createFakeClock } from "../background/support.ts";
import { createFakeVariantView, type FakeNode } from "./fake-ad-dom.ts";
import { ashdiSeriesPage, flatPlaylistPage, qualityOnlyPage, tortugaPage, type FakeDub, type FakeSeason } from "./fake-playerjs-page.ts";

/** Обе стороны руки, соединённые подделкой порта (сообщения идут асинхронно, как настоящие). */
function wireHand(page: object): AdapterHandClient {
  const clock = createFakeClock();
  let client: AdapterHandClient | undefined;
  const executor = createAdapterHandExecutor({ page, send: (reply) => queueMicrotask(() => client?.acceptPageMessage(structuredClone(reply))) });
  client = createAdapterHandClient({
    sendToPage: (call) => queueMicrotask(() => void executor.handle(structuredClone(call))),
    timers: clock.scheduler,
    now: clock.scheduler.now,
  });
  return client;
}

/** Пауза адаптера: страница дорисовывает списки, потом (по желанию теста) что-то ещё. */
function pauseThat(onSleep: () => void | Promise<void>): PlayerJsPause & { sleeps: number } {
  const pause = {
    sleeps: 0,
    async sleep(): Promise<void> {
      pause.sleeps += 1;
      await onSleep();
    },
  };
  return pause;
}

function adapterFor(page: { root: FakeNode; window: object; flush(): void }, hostname: string, onSleep?: () => void | Promise<void>) {
  const view = createFakeVariantView(page.root, { hostname });
  const pause = pauseThat(onSleep ?? (() => page.flush()));
  const adapter = createPlayerJsVariants({ dom: view, player: wireHand(page.window).handFor(PLAYERJS_SITE.id) }, pause);
  return { adapter, view, pause };
}

const episodes = (count: number, firstFileId: number) => Array.from({ length: count }, (_, index) => ({ label: `Серія ${index + 1}`, fileId: firstFileId + index }));

/** «Гранд тур» на Tortuga (разведка): два сезона, две озвучки-дорожки. */
const GRAND_TOUR: readonly FakeSeason[] = [
  { label: "Сезон 1", episodes: episodes(3, 1) },
  { label: "Сезон 2", episodes: episodes(2, 10) },
];

/** Сериал Ashdi `?multivoice`: у «Sunnysiders» два сезона, у «Цікава Ідея» — только первый. */
const ASHDI_DUBS: readonly FakeDub[] = [
  { label: "DniproFilm", seasons: [{ label: "Сезон 1", episodes: episodes(3, 100) }, { label: "Сезон 2", episodes: episodes(2, 200) }] },
  { label: "Sunnysiders", seasons: [{ label: "Сезон 1", episodes: episodes(3, 300) }, { label: "Сезон 2", episodes: episodes(2, 400) }] },
  { label: "Цікава Ідея", seasons: [{ label: "Сезон 1", episodes: episodes(3, 500) }] },
];

function optionIdOf(description: PageVariantDescription | null, kind: "audio" | "quality", label: string): string {
  const option = description?.[kind].find((candidate) => candidate.label === label);
  assert.ok(option, `в меню ${kind} нет «${label}»`);
  return option.id;
}

describe("PlayerJS: опознание и рука-функция", () => {
  it("свой во фрейме с разметкой PlayerJS на любом хосте (решение 290); без неё — нет", () => {
    const page = qualityOnlyPage({ qualities: ["480p", "720p"], quality: "480p" });
    const { adapter } = adapterFor(page, "player.example.org");
    assert.equal(adapter.matches(createFakeVariantView(page.root, { hostname: "player.example.org" })), true);
    const empty = createFakeVariantView(new (page.root.constructor as typeof FakeNode)("html"), { hostname: "ashdi.vip" });
    assert.equal(adapter.matches(empty), false);
  });

  it("белый список руки зарегистрирован в мире страницы: иначе каждый вызов был бы `no_such_action`", () => {
    assert.deepEqual(Object.keys(playerHandActionsOf(PLAYERJS_SITE.id) ?? {}).sort(), ["describe_player", "next_item", "select_audio_track", "select_quality"]);
  });

  it("плеер ещё не построен (до клика `api()` отвечает false) — «сейчас ничего», без меню", async () => {
    const page = qualityOnlyPage({ qualities: false, quality: false, ready: false });
    assert.equal(await adapterFor(page, "ashdi.vip").adapter.describe(), null);
  });

  it("во фрейме нет `pljssglobal` — «сейчас ничего»", async () => {
    const page = qualityOnlyPage({ qualities: ["480p"], quality: "480p" });
    const view = createFakeVariantView(page.root, { hostname: "ashdi.vip" });
    const adapter = createPlayerJsVariants({ dom: view, player: wireHand({ location: { pathname: "/", search: "" } }).handFor(PLAYERJS_SITE.id) });
    assert.equal(await adapter.describe(), null);
  });

  it("`api()` бросает — описание срывается целиком (адаптер падает, видео играет перехватом)", async () => {
    const view = createFakeVariantView(qualityOnlyPage({ qualities: [], quality: "" }).root, { hostname: "ashdi.vip" });
    const broken = { location: { pathname: "/", search: "" }, pljssglobal: [{ api: () => { throw new Error("player crashed"); } }] };
    const adapter = createPlayerJsVariants({ dom: view, player: wireHand(broken).handFor(PLAYERJS_SITE.id) });
    await assert.rejects(adapter.describe(), PlayerJsPageError);
  });
});

describe("PlayerJS: качество (uakino, Ashdi без плейлиста)", () => {
  it("качества из `api(\"qualities\")`, «Авто» помечено automatic, ключ сериала — хост и путь фрейма, серии нет", async () => {
    const page = qualityOnlyPage({ qualities: ["480p", "720p", "1080p", "Авто"], quality: "480p" });
    const description = await adapterFor(page, "ashdi.vip").adapter.describe();
    assert.equal(description?.seriesKey, "ashdi.vip/vod/300629");
    assert.equal(description?.episodeKey, null);
    assert.deepEqual(description?.audio, []);
    assert.equal(description?.nextEpisode, false);
    assert.deepEqual(description?.quality.map((option) => [option.label, option.selected, option.automatic ?? false]), [
      ["480p", true, false],
      ["720p", false, false],
      ["1080p", false, false],
      ["Авто", false, true],
    ]);
  });

  it("выбор качества зовёт `api(\"quality\", номер)` и страница встаёт на него", async () => {
    const page = qualityOnlyPage({ qualities: ["480p", "720p", "1080p", "Авто"], quality: "480p" });
    const { adapter } = adapterFor(page, "ashdi.vip");
    const id = optionIdOf(await adapter.describe(), "quality", "1080p");
    assert.equal(await adapter.select("quality", id), "done");
    assert.ok(page.log.apiCalls.includes("quality:2"));
    assert.equal(page.api.quality, "1080p");
    assert.equal((await adapter.describe())?.quality.find((option) => option.selected)?.label, "1080p");
  });

  it("«Авто»: PlayerJS называет качество «Авто 1080p» — в меню выбран автоматический пункт, а не «1080p»", async () => {
    const page = qualityOnlyPage({ qualities: ["480p", "720p", "1080p", "Авто"], quality: "480p" });
    const { adapter } = adapterFor(page, "ashdi.vip");
    assert.equal(await adapter.select("quality", optionIdOf(await adapter.describe(), "quality", "Авто")), "done");
    assert.equal(page.api.quality, "Авто 1080p");
    const selected = (await adapter.describe())?.quality.filter((option) => option.selected);
    assert.deepEqual(selected?.map((option) => [option.label, option.automatic]), [["Авто", true]]);
  });

  it("устаревший id (на этом месте уже другая подпись) — «такого нет», качество не тронуто", async () => {
    const page = qualityOnlyPage({ qualities: ["480p", "720p", "1080p"], quality: "480p" });
    const { adapter } = adapterFor(page, "ashdi.vip");
    const id = optionIdOf(await adapter.describe(), "quality", "1080p");
    page.api.qualities = ["480p", "720p", "1440p"]; // сайт пересобрал список
    assert.equal(await adapter.select("quality", id), "no_such_variant");
    assert.equal(page.api.quality, "480p");
    assert.ok(!page.log.apiCalls.some((call) => call.startsWith("quality:")));
  });

  it("одно качество или не строки (`[1]` у Ashdi без выбора) — меню качеств нет", async () => {
    assert.deepEqual((await adapterFor(qualityOnlyPage({ qualities: [1], quality: "1" }), "ashdi.vip").adapter.describe())?.quality, []);
    assert.deepEqual((await adapterFor(qualityOnlyPage({ qualities: ["720p"], quality: "720p" }), "ashdi.vip").adapter.describe())?.quality, []);
  });
});

describe("PlayerJS: Tortuga — сезоны, серии, озвучки-дорожки", () => {
  it("описание: озвучки из `audio_N`, ключ серии «сезон/серия», Next есть", async () => {
    const page = tortugaPage({ seasons: GRAND_TOUR, dubs: ["В одне рило", "Субтитри"] });
    const description = await adapterFor(page, "tortuga.tw").adapter.describe();
    assert.equal(description?.seriesKey, "tortuga.tw/embed/869");
    assert.equal(description?.episodeKey, "Сезон 1/Серія 1");
    assert.deepEqual(description?.audio.map((option) => [option.label, option.selected]), [["В одне рило", true], ["Субтитри", false]]);
    assert.equal(description?.nextEpisode, true);
    for (const option of [...(description?.audio ?? []), ...(description?.quality ?? [])]) assert.match(option.id, /^[A-Za-z0-9_-]{1,64}$/u);
  });

  it("смена озвучки — `api(\"audiotrack\", номер)`: та же серия, новая озвучка, ключ серии прежний", async () => {
    const page = tortugaPage({ seasons: GRAND_TOUR, dubs: ["В одне рило", "Субтитри"], playing: { season: 0, episode: 2 } });
    const { adapter } = adapterFor(page, "tortuga.tw");
    const id = optionIdOf(await adapter.describe(), "audio", "Субтитри");
    assert.equal(await adapter.select("audio", id), "done");
    assert.ok(page.log.apiCalls.includes("audiotrack:1"));
    const after = await adapter.describe();
    assert.equal(after?.audio.find((option) => option.selected)?.label, "Субтитри");
    assert.equal(after?.episodeKey, "Сезон 1/Серія 3", "смена озвучки — не новая серия");
  });

  it("Next — `api(\"next\")`: следующая серия, через конец сезона — первая серия следующего", async () => {
    const page = tortugaPage({ seasons: GRAND_TOUR, dubs: ["В одне рило"], playing: { season: 0, episode: 2 } });
    const { adapter } = adapterFor(page, "tortuga.tw");
    assert.equal((await adapter.describe())?.nextEpisode, true, "последняя серия сезона, но есть следующий сезон");
    assert.equal(await adapter.next(), "done");
    assert.equal((await adapter.describe())?.episodeKey, "Сезон 2/Серія 1");
  });

  it("последняя серия последнего сезона — Next нет, и `api(\"next\")` не зовётся", async () => {
    const page = tortugaPage({ seasons: GRAND_TOUR, dubs: ["В одне рило"], playing: { season: 1, episode: 1 } });
    const { adapter } = adapterFor(page, "tortuga.tw");
    assert.equal((await adapter.describe())?.nextEpisode, false);
    assert.equal(await adapter.next(), "no_such_variant");
    assert.ok(!page.log.apiCalls.includes("next"));
  });
});

describe("PlayerJS: сериал Ashdi `?multivoice` — озвучки-папки", () => {
  it("описание по тому, что играет: озвучка без ведущего пробела, ключ серии — подписи, не id файла", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 0 } });
    const description = await adapterFor(page, "ashdi.vip").adapter.describe();
    assert.equal(description?.seriesKey, "ashdi.vip/serial/6320");
    assert.equal(description?.episodeKey, "Сезон 2/Серія 1");
    assert.deepEqual(description?.audio.map((option) => [option.label, option.selected]), [["DniproFilm", true], ["Sunnysiders", false], ["Цікава Ідея", false]]);
    assert.equal(description?.nextEpisode, true);
  });

  it("человек открыл на странице чужую папку — меню «на полпути», описание прежнее (то, что играет)", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 0 } });
    const { adapter } = adapterFor(page, "ashdi.vip");
    const before = await adapter.describe();
    page.openDubFolderByHand(1);
    assert.deepEqual(await adapter.describe(), before);
  });

  it("смена озвучки — три нажатия: озвучка → тот же сезон → та же серия; играет новая озвучка, серия та же", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 1 } });
    const { adapter } = adapterFor(page, "ashdi.vip");
    const id = optionIdOf(await adapter.describe(), "audio", "Sunnysiders");
    assert.equal(await adapter.select("audio", id), "done");
    assert.deepEqual(page.presses, ["dub:Sunnysiders", "season:Сезон 2", "episode:Серія 2"]);
    page.flush();
    assert.deepEqual(page.playing, { dub: 1, season: 1, episode: 1 });
    const after = await adapter.describe();
    assert.equal(after?.audio.find((option) => option.selected)?.label, "Sunnysiders");
    assert.equal(after?.episodeKey, "Сезон 2/Серія 2");
  });

  it("пока идут три нажатия, описание — последнее устоявшееся, а не открытые папки", async () => {
    let page: ReturnType<typeof ashdiSeriesPage> | undefined;
    let adapter: ReturnType<typeof createPlayerJsVariants> | undefined;
    const seenDuringSwitch: (PageVariantDescription | null)[] = [];
    page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 1 } });
    const built = adapterFor(page, "ashdi.vip", async () => {
      page?.flush();
      seenDuringSwitch.push((await adapter?.describe()) ?? null);
    });
    adapter = built.adapter;
    const before = await adapter.describe();
    assert.equal(await adapter.select("audio", optionIdOf(before, "audio", "Sunnysiders")), "done");
    assert.ok(seenDuringSwitch.length >= 2, "адаптер ждал перерисовку после каждого открытия папки");
    for (const seen of seenDuringSwitch) assert.deepEqual(seen, before);
  });

  it("выбор уже играющей озвучки — «сделано» без нажатий: нажать играющую серию значило бы начать её заново", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 1 } });
    const { adapter } = adapterFor(page, "ashdi.vip");
    assert.equal(await adapter.select("audio", optionIdOf(await adapter.describe(), "audio", "DniproFilm")), "done");
    assert.deepEqual(page.presses, []);
  });

  it("вторая команда посреди смены в три нажатия отклоняется: нажатия двух смен не перемешиваются", async () => {
    let adapter: ReturnType<typeof createPlayerJsVariants> | undefined;
    let secondOutcome: string | undefined;
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 1 } });
    let description: PageVariantDescription | null = null;
    const built = adapterFor(page, "ashdi.vip", async () => {
      page.flush();
      secondOutcome ??= await adapter?.select("audio", optionIdOf(description, "audio", "Цікава Ідея"));
    });
    adapter = built.adapter;
    description = await adapter.describe();
    assert.equal(await adapter.select("audio", optionIdOf(description, "audio", "Sunnysiders")), "done");
    assert.equal(secondOutcome, "no_such_variant");
    assert.deepEqual(page.presses, ["dub:Sunnysiders", "season:Сезон 2", "episode:Серія 2"]);
  });

  it("у новой озвучки нет текущего сезона — остаёмся где были (решение 295): серия не нажата, меню страницы возвращено", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 1, episode: 0 } });
    const { adapter } = adapterFor(page, "ashdi.vip");
    const before = await adapter.describe();
    assert.equal(await adapter.select("audio", optionIdOf(before, "audio", "Цікава Ідея")), "no_such_episode");
    assert.ok(!page.presses.some((press) => press.startsWith("episode:")), "играющую серию не трогали");
    assert.deepEqual(page.playing, { dub: 0, season: 1, episode: 0 });
    page.flush();
    assert.deepEqual(page.open, { dub: 0, season: 1 }, "открыты папки играющей озвучки и сезона");
    assert.deepEqual(await adapter.describe(), before);
  });

  it("страница не перерисовала папку за отведённое время — «сбой», меню возвращено, серия не нажата", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 0, season: 0, episode: 0 } });
    // Страница «замёрзла»: нажатие принято, но списки не перерисовываются.
    const { adapter, pause } = adapterFor(page, "ashdi.vip", () => undefined);
    const id = optionIdOf(await adapter.describe(), "audio", "Sunnysiders");
    const outcome = adapter.select("audio", id);
    assert.equal(await outcome, "failed");
    assert.ok(pause.sleeps > 20, "ждал опросами, а не одним взглядом");
    assert.ok(!page.presses.some((press) => press.startsWith("episode:")));
  });

  it("Next внутри озвучки: с последней серии сезона — в следующий сезон; за последним сезоном Next нет", async () => {
    const page = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 1, season: 0, episode: 2 } });
    const { adapter } = adapterFor(page, "ashdi.vip");
    assert.equal((await adapter.describe())?.nextEpisode, true);
    assert.equal(await adapter.next(), "done");
    assert.equal((await adapter.describe())?.episodeKey, "Сезон 2/Серія 1");
    const last = ashdiSeriesPage({ dubs: ASHDI_DUBS, playing: { dub: 1, season: 1, episode: 1 } });
    const lastAdapter = adapterFor(last, "ashdi.vip").adapter;
    assert.equal((await lastAdapter.describe())?.nextEpisode, false, "дальше — уже другая озвучка");
    assert.equal(await lastAdapter.next(), "no_such_variant");
    assert.ok(!last.log.apiCalls.includes("next"));
  });
});

describe("PlayerJS: ровный список", () => {
  it("фильм Ashdi `?multivoice`: озвучки — пункты, выбор — одно нажатие, ключ серии `null`, Next нет", async () => {
    const page = flatPlaylistPage({
      labels: [" професійний дубльований", "двоголосий закадровий"],
      fileIds: [300629, 291665],
      playing: 0,
      pathname: "/vod/300629",
      search: "?multivoice",
    });
    const { adapter, view } = adapterFor(page, "ashdi.vip");
    const description = await adapter.describe();
    assert.equal(description?.episodeKey, null);
    assert.equal(description?.nextEpisode, false, "`api(\"next\")` у фильма включил бы следующую озвучку");
    assert.deepEqual(description?.audio.map((option) => [option.label, option.selected]), [["професійний дубльований", true], ["двоголосий закадровий", false]]);
    assert.equal(await adapter.select("audio", optionIdOf(description, "audio", "двоголосий закадровий")), "done");
    page.flush();
    assert.equal(page.state.playing, 1);
    assert.equal((await adapter.describe())?.audio.find((option) => option.selected)?.label, "двоголосий закадровий");
    assert.equal(await adapter.next(), "no_such_variant");
    assert.ok(view.find("pjsdiv[me]"));
  });

  it("незнакомый сайт: пункты похожи на серии — Next есть, озвучек нет (решение 296)", async () => {
    const page = flatPlaylistPage({ labels: ["Серія 1", "Серія 2", "Серія 3"], playing: 0, pathname: "/embed/7", search: "?multivoice" });
    const { adapter } = adapterFor(page, "player.example.org");
    const description = await adapter.describe();
    assert.deepEqual(description?.audio, []);
    assert.equal(description?.episodeKey, "Серія 1");
    assert.equal(description?.nextEpisode, true);
    assert.equal(await adapter.next(), "done");
    assert.equal((await adapter.describe())?.episodeKey, "Серія 2");
  });

  it("незнакомый сайт: пункты не похожи на серии — ни озвучек, ни Next; качество остаётся", async () => {
    const page = flatPlaylistPage({ labels: ["Dub A", "Dub B"], playing: 0, pathname: "/embed/7", search: "?multivoice" });
    const { adapter } = adapterFor(page, "player.example.org");
    const description = await adapter.describe();
    assert.deepEqual(description?.audio, []);
    assert.equal(description?.nextEpisode, false);
    assert.equal(description?.episodeKey, null);
    assert.equal(description?.quality.length, 2);
    assert.equal(await adapter.select("audio", "x-1_abc"), "no_such_variant");
    assert.equal(page.state.playing, 0);
  });

  it("Ashdi без `?multivoice`: ровный список — серии, а не озвучки", async () => {
    const page = flatPlaylistPage({ labels: ["Серія 1", "Серія 2"], fileIds: [11, 12], playing: 0, pathname: "/serial/5", search: "" });
    const description = await adapterFor(page, "ashdi.vip").adapter.describe();
    assert.deepEqual(description?.audio, []);
    assert.equal(description?.episodeKey, "Серія 1");
    assert.equal(description?.nextEpisode, true);
  });
});

describe("PlayerJS: команды вне меню", () => {
  it("меню субтитров у PlayerJS нет; негодный id — «такого нет», страница не тронута", async () => {
    const page = tortugaPage({ seasons: GRAND_TOUR, dubs: ["В одне рило", "Субтитри"] });
    const { adapter } = adapterFor(page, "tortuga.tw");
    await adapter.describe();
    const callsBefore = page.log.apiCalls.length;
    assert.equal(await adapter.select("subtitles", "x-0_abc"), "no_such_variant");
    assert.equal(await adapter.select("audio", "без-отпечатка"), "no_such_variant");
    assert.equal(await adapter.select("audio", "audio_7_abc"), "no_such_variant");
    assert.equal(page.state.dub, 0);
    assert.ok(!page.log.apiCalls.slice(callsBefore).some((call) => call.startsWith("audiotrack")));
  });
});
