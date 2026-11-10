// Адаптер вариантов страницы «сериал» стенда (14b) по фикстуре её устройства — с НАСТОЯЩИМИ обеими руками:
// клик по DOM (подделка дерева) и рука-функция (настоящий клиент изолированного мира + настоящий исполнитель
// MAIN-мира, соединённые подделкой личного порта). Плеер страницы — маленький объект в «окне».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { VariantPageView } from "../../src/adapters/capabilities/variants/contract.ts";
import { playerHandActionsOf } from "../../src/adapters/registry/page-world.ts";
import { BENCH_SERIES_SITE } from "../../src/adapters/sites/bench-series/identity.ts";
import { BenchSeriesPageError, createBenchSeriesVariants } from "../../src/adapters/sites/bench-series/variants.ts";
import { createAdapterHandClient, type AdapterHandClient } from "../../src/content/adapter-hand-client.ts";
import { createAdapterHandExecutor } from "../../src/page/adapter-hand.ts";
import { createFakeClock, flushPromises } from "../background/support.ts";
import { createFakeAdView, FakeNode } from "./fake-ad-dom.ts";

/** Фикстура: устройство `series.html` стенда без личных данных — корень с меткой, ключи, кнопки озвучек. */
function benchSeriesPage(options: { selectedDub: "dub-a" | "dub-b"; episode: string; marker?: boolean }) {
  const dubA = new FakeNode("button", { text: "Dub A", attributes: { "data-dub": "dub-a", "aria-pressed": String(options.selectedDub === "dub-a") } });
  const dubB = new FakeNode("button", { text: "Dub B", attributes: { "data-dub": "dub-b", "aria-pressed": String(options.selectedDub === "dub-b") } });
  const markerAttributes = options.marker === false ? {} : { "data-fastiplayer-bench-series": "", "data-series-key": "bench-series", "data-episode-key": options.episode };
  const controls = new FakeNode("div", { id: "controls", attributes: markerAttributes }).add(dubA, dubB);
  const root = new FakeNode("body").add(new FakeNode("video", { id: "v" }), controls);
  return { root, dubA, dubB };
}

function variantView(root: FakeNode, hostname = "127.0.0.1"): VariantPageView {
  return { ...createFakeAdView(root, { hostname }), attributeOf: (element, name) => (element as FakeNode).attributes.get(name) ?? null };
}

/** Плеер страницы в её окне: то, что видит рука-функция. Ломается по просьбе сценария. */
function fakePagePlayer(options: { broken?: boolean; autoQuality?: unknown } = {}) {
  const calls: string[] = [];
  const state = { quality: "video-360", episode: 1 };
  const player = {
    qualities() {
      calls.push("qualities");
      if (options.broken === true) throw new Error("page player crashed");
      const fixed = [
        { id: "video-360", label: "360p", selected: state.quality === "video-360" },
        { id: "video-720", label: "720p", selected: state.quality === "video-720" },
      ];
      // «Auto» сайта (14b2) — по просьбе сценария, с пометкой какого угодно вида.
      return options.autoQuality === undefined ? fixed : [{ id: "auto", label: "Auto", selected: false, automatic: options.autoQuality }, ...fixed];
    },
    selectQuality(id: string) {
      calls.push(`selectQuality:${id}`);
      if (id !== "video-360" && id !== "video-720") return false;
      state.quality = id;
      return true;
    },
    hasNextEpisode: () => state.episode < 3,
    nextEpisode() {
      calls.push("nextEpisode");
      if (state.episode >= 3) return false;
      state.episode += 1;
      return true;
    },
  };
  return { window: { fastiplayerBenchSeries: player } as object, calls, state };
}

/** Обе стороны руки, соединённые подделкой порта (сообщения идут асинхронно, как настоящие). */
function wireHand(page: object): { client: AdapterHandClient; clock: ReturnType<typeof createFakeClock> } {
  const clock = createFakeClock();
  let client: AdapterHandClient | undefined;
  const executor = createAdapterHandExecutor({ page, send: (reply) => queueMicrotask(() => client?.acceptPageMessage(structuredClone(reply))) });
  client = createAdapterHandClient({
    sendToPage: (call) => queueMicrotask(() => void executor.handle(structuredClone(call))),
    timers: clock.scheduler,
    now: clock.scheduler.now,
  });
  return { client, clock };
}

function adapterFor(root: FakeNode, page: object, hostname = "127.0.0.1") {
  const { client } = wireHand(page);
  const view = variantView(root, hostname);
  return { adapter: createBenchSeriesVariants({ dom: view, player: client.handFor(BENCH_SERIES_SITE.id) }), view };
}

describe("адаптер стенда «сериал»: опознание", () => {
  it("свой только на локальном хосте стенда и только с меткой страницы", () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    const player = fakePagePlayer();
    assert.equal(adapterFor(page.root, player.window).adapter.matches(variantView(page.root)), true);
    assert.equal(adapterFor(page.root, player.window).adapter.matches(variantView(page.root, "localhost")), true);
    // Живой сайт с такой же разметкой — не наш: адаптер стенда в продукте никого не трогает.
    assert.equal(adapterFor(page.root, player.window).adapter.matches(variantView(page.root, "www.example.com")), false);
    const unmarked = benchSeriesPage({ selectedDub: "dub-a", episode: "1", marker: false });
    assert.equal(adapterFor(unmarked.root, player.window).adapter.matches(variantView(unmarked.root)), false);
  });
});

describe("адаптер стенда «сериал»: описание через обе руки", () => {
  it("озвучки — из разметки, качества и «следующая серия» — из функций плеера страницы", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-b", episode: "2" });
    const player = fakePagePlayer();
    const description = await adapterFor(page.root, player.window).adapter.describe();
    assert.deepEqual(description, {
      seriesKey: "bench-series",
      episodeKey: "2",
      audio: [
        { id: "dub-a", label: "Dub A", selected: false },
        { id: "dub-b", label: "Dub B", selected: true },
      ],
      quality: [
        { id: "video-360", label: "360p", selected: true },
        { id: "video-720", label: "720p", selected: false },
      ],
      subtitles: [],
      nextEpisode: true,
    });
    // Рука-функция дошла до плеера страницы (через исполнитель MAIN-мира), а не придумала ответ.
    assert.deepEqual(player.calls, ["qualities"]);
  });

  it("«Auto» сайта приходит с пометкой automatic; пометка не булева — описание срывается (14b2)", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    const described = await adapterFor(page.root, fakePagePlayer({ autoQuality: true }).window).adapter.describe();
    assert.deepEqual(described?.quality[0], { id: "auto", label: "Auto", selected: false, automatic: true });
    assert.deepEqual(described?.quality[1], { id: "video-360", label: "360p", selected: true }, "у обычного пункта пометки нет");
    await assert.rejects(adapterFor(page.root, fakePagePlayer({ autoQuality: "yes" }).window).adapter.describe(), /quality automatic/u);
  });

  it("плеер страницы сломан (функция бросает) — описание срывается целиком, полменю не бывает", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    await assert.rejects(adapterFor(page.root, fakePagePlayer({ broken: true }).window).adapter.describe(), BenchSeriesPageError);
  });

  it("у страницы нет объекта плеера — тоже срыв, а не пустые качества", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    await assert.rejects(adapterFor(page.root, {}).adapter.describe(), /qualities: no_such_action/u);
  });

  it("корня с меткой нет (страница ещё строится) — честное «сейчас ничего»", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1", marker: false });
    assert.equal(await adapterFor(page.root, fakePagePlayer().window).adapter.describe(), null);
  });
});

describe("адаптер стенда «сериал»: действия", () => {
  it("озвучка — клик ровно по своей кнопке; чужой ключ и ключ с символами селектора ничего не нажимают", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    const { adapter } = adapterFor(page.root, fakePagePlayer().window);
    assert.equal(await adapter.select("audio", "dub-b"), "done");
    assert.deepEqual([page.dubA.clicks, page.dubB.clicks], [0, 1]);
    assert.equal(await adapter.select("audio", "dub-z"), "no_such_variant");
    assert.equal(await adapter.select("audio", '"], button, [data-dub="dub-a'), "no_such_variant");
    assert.deepEqual([page.dubA.clicks, page.dubB.clicks], [0, 1]);
  });

  it("качество — вызов функции страницы с ключом; страница сказала «нет такого» — no_such_variant", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    const player = fakePagePlayer();
    const { adapter } = adapterFor(page.root, player.window);
    assert.equal(await adapter.select("quality", "video-720"), "done");
    assert.equal(player.state.quality, "video-720");
    assert.equal(await adapter.select("quality", "video-4k"), "no_such_variant");
    assert.equal(player.state.quality, "video-720");
    // Клика по озвучкам при выборе качества нет.
    assert.deepEqual([page.dubA.clicks, page.dubB.clicks], [0, 0]);
  });

  it("следующая серия — вызов функции; на последней серии — no_such_variant; плеера нет — failed", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    const player = fakePagePlayer();
    const { adapter } = adapterFor(page.root, player.window);
    assert.equal(await adapter.next(), "done");
    assert.equal(await adapter.next(), "done");
    assert.equal(await adapter.next(), "no_such_variant");
    assert.equal(player.state.episode, 3);
    assert.equal(await adapterFor(page.root, {}).adapter.next(), "failed");
  });
});

describe("белый список руки-функции", () => {
  it("исполнитель MAIN-мира зовёт только действия адаптера: чужой сайт, `constructor`, `toString` — no_such_action", async () => {
    const player = fakePagePlayer();
    const replies: unknown[] = [];
    const executor = createAdapterHandExecutor({ page: player.window, send: (reply) => void replies.push(reply.outcome) });
    await executor.handle({ kind: "adapter_hand_call", request_id: 1, site: "youtube", action: "qualities", argument: null });
    await executor.handle({ kind: "adapter_hand_call", request_id: 2, site: BENCH_SERIES_SITE.id, action: "constructor", argument: null });
    await executor.handle({ kind: "adapter_hand_call", request_id: 3, site: BENCH_SERIES_SITE.id, action: "tostring", argument: null });
    assert.deepEqual(replies, [{ kind: "no_such_action" }, { kind: "no_such_action" }, { kind: "no_such_action" }]);
    assert.deepEqual(player.calls, [], "до плеера страницы не дошло ничего");
    assert.ok(playerHandActionsOf(BENCH_SERIES_SITE.id) !== undefined);
  });

  it("результат страницы с прототипом, функцией или геттером-ловушкой — failed, а не объект страницы", async () => {
    const results: unknown[] = [
      new Map([["a", 1]]),
      { label: () => "x" },
      Object.defineProperty({}, "id", { enumerable: true, get: () => { throw new Error("trap"); } }),
    ];
    for (const result of results) {
      const replies: unknown[] = [];
      const page = { fastiplayerBenchSeries: { qualities: () => result } };
      await createAdapterHandExecutor({ page, send: (reply) => void replies.push(reply.outcome) }).handle({
        kind: "adapter_hand_call",
        request_id: 1,
        site: BENCH_SERIES_SITE.id,
        action: "qualities",
        argument: null,
      });
      assert.deepEqual(replies, [{ kind: "failed" }]);
    }
  });

  it("вечное обещание страницы не держит адаптер: через срок — no_answer, действие — failed", async () => {
    const page = benchSeriesPage({ selectedDub: "dub-a", episode: "1" });
    const { client, clock } = wireHand({ fastiplayerBenchSeries: { nextEpisode: () => new Promise(() => {}) } });
    const adapter = createBenchSeriesVariants({ dom: variantView(page.root), player: client.handFor(BENCH_SERIES_SITE.id) });
    const outcome = adapter.next();
    await flushPromises();
    clock.advance(2_000);
    assert.equal(await outcome, "failed");
    assert.equal(client.counters().unanswered, 1);
  });
});
