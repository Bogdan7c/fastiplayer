// Классификатор, снимок и модель вкладки (09c, решение 198) на записанных последовательностях
// фактов стенда: ad-preroll, banner-autoplay, series, Ashdi со скрытым играющим зеркалом,
// Shaka-опрос ключей и Vimeo `setMediaKeys` без `encrypted`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { classifyVideo } from "../../src/background/tab-model/video-verdict.ts";
import { createMemoryTabModelStore, type TabModelStore } from "../../src/background/tab-model/session-store.ts";
import { createTabModel, type TabModel } from "../../src/background/tab-model/tab-model.ts";
import { MAX_TAB_VIDEO_RECORDS } from "../../src/background/tab-model/tab-snapshot.ts";
import { followTabLifecycle, type TabsLifecycleApi } from "../../src/background/tab-model/tab-lifecycle.ts";
import type { FrameCandidates } from "../../src/background/handoff/video-choice.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";

function video(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { document: "doc-top", element, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 400_000, ...DEFAULT_CANDIDATE_FACTS, ...overrides };
}

/** Дать отработать цепочке обещаний переноса записи. */
async function flushMicrotasks(): Promise<void> {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
}

function modelOver(store: TabModelStore, failures: unknown[] = []): TabModel {
  return createTabModel({ store, now: () => 1234, reportStoreFailure: (error) => void failures.push(error) });
}

describe("классификатор видео", () => {
  it("DRM важнее всего: защищённый элемент с рекламным хостом всё равно protected", () => {
    assert.equal(classifyVideo(video(1, { drm: true, frame_host: "imasdk.googleapis.com" })), "protected");
  });

  it("нет источника — empty, неподдержанная форма — unsupported", () => {
    assert.equal(classifyVideo(video(1, { media: { kind: "none" } })), "empty");
    assert.equal(classifyVideo(video(1, { media: { kind: "unsupported", form: "blob_file" } })), "unsupported");
  });

  it("файлы рекламной сети Google на 2mdn.net — реклама", () => {
    assert.equal(classifyVideo(video(1, { media: { kind: "file" }, file_host: "s0.2mdn.net" })), "ad");
  });

  it("рекламный хост файла или документа (и поддомен) — ad; похожий чужой домен — нет", () => {
    assert.equal(classifyVideo(video(1, { media: { kind: "file" }, file_host: "pubads.g.doubleclick.net" })), "ad");
    assert.equal(classifyVideo(video(1, { frame_host: "IMASDK.googleapis.com" })), "ad");
    assert.equal(classifyVideo(video(1, { frame_host: "notdoubleclick.net" })), "main");
    assert.equal(classifyVideo(video(1, { media: { kind: "file" }, file_host: "cdn.site.example" })), "main");
  });

  it("беззвучное: цикл, мелкая плитка или короткий ролик — junk; беззвучный фильм — main", () => {
    assert.equal(classifyVideo(video(1, { muted: true, loop: true })), "junk");
    assert.equal(classifyVideo(video(1, { muted: true, visible_area_px: 100 * 60 })), "junk");
    assert.equal(classifyVideo(video(1, { muted: true, duration_s: 8 })), "junk");
    assert.equal(classifyVideo(video(1, { muted: true })), "main");
  });

  it("известный короткий ролик на паузе — junk, играющий со звуком — main", () => {
    assert.equal(classifyVideo(video(1, { duration_s: 10 })), "junk");
    assert.equal(classifyVideo(video(1, { duration_s: 10, playing: true })), "main");
    assert.equal(classifyVideo(video(1, { duration_s: null })), "main");
  });
});

/** Записанный набор «преролл-реклама рядом с фильмом» (стенд ad-preroll). */
const AD_PREROLL: FrameCandidates[] = [
  {
    frameId: 0,
    candidates: [
      video(1, { duration_s: 5400, visible_area_px: 640_000 }),
      video(2, { media: { kind: "file" }, file_host: "ads.doubleclick.net", playing: true, duration_s: 20, visible_area_px: 640_000 }),
    ],
  },
];

describe("модель вкладки на записанных наборах", () => {
  it("ad-preroll: главное — фильм, рекламный элемент размечен как реклама", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(7, AD_PREROLL);
    assert.deepEqual(await model.mainVideoOf(7), { frame_id: 0, document: "doc-top", element: 1 });
    assert.equal(await model.isAdOn(7, { frame_id: 0, document: "doc-top", element: 2 }), true);
    assert.equal(await model.isAdOn(7, { frame_id: 0, document: "doc-top", element: 1 }), false);
    assert.equal(await model.isAdOn(7, { frame_id: 5, document: "doc-top", element: 2 }), false, "чужой фрейм — другое видео");
    assert.equal(await model.isAdOn(99, { frame_id: 0, document: "doc-top", element: 2 }), false, "неопрошенная вкладка");
  });

  it("реклама, не играющая сейчас, не считается идущей", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(7, [{ frameId: 0, candidates: [video(2, { media: { kind: "file" }, file_host: "x.doubleclick.net", playing: false })] }]);
    assert.equal(await model.isAdOn(7, { frame_id: 0, document: "doc-top", element: 2 }), false);
  });

  it("banner-autoplay: баннер и превью — junk, главным остаётся плеер", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(3, [
      {
        frameId: 0,
        candidates: [
          video(1, { playing: true, muted: true, loop: true, visible_area_px: 300 * 250, duration_s: 15 }),
          video(2, { muted: true, visible_area_px: 120 * 68 }),
          video(3, { visible_area_px: 800_000 }),
        ],
      },
    ]);
    const verdicts = (await model.videosFoundIn(3)).map((record) => [record.element, record.verdict]);
    assert.deepEqual(verdicts, [[3, "main"], [1, "junk"], [2, "junk"]]);
    assert.deepEqual(await model.mainVideoOf(3), { frame_id: 0, document: "doc-top", element: 3 });
  });

  it("Ashdi: скрытое играющее зеркало не главное, пока виден плеер", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(4, [
      { frameId: 0, candidates: [video(1, { visible_area_px: 500_000 })] },
      { frameId: 2, candidates: [video(1, { document: "doc-mirror", playing: true, visible_area_px: 0 })] },
    ]);
    assert.deepEqual(await model.mainVideoOf(4), { frame_id: 0, document: "doc-top", element: 1 });
    assert.equal((await model.videosFoundIn(4)).length, 2, "зеркало остаётся в списке для попапа");
  });

  it("series: новый опрос заменяет прежний снимок и сбрасывает отказ", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(5, [{ frameId: 0, candidates: [video(1, { document: "doc-ep1" })] }]);
    await model.noteHandoffRefused(5, { kind: "player_unavailable" });
    assert.deepEqual(await model.lastRefusalOf(5), { kind: "player_unavailable" });
    await model.recordSurvey(5, [{ frameId: 0, candidates: [video(1, { document: "doc-ep2" })] }]);
    assert.deepEqual(await model.mainVideoOf(5), { frame_id: 0, document: "doc-ep2", element: 1 });
    assert.equal(await model.lastRefusalOf(5), null);
  });

  it("Shaka опрашивает ключи (drm=false) — видео передаваемо; Vimeo setMediaKeys без encrypted (drm=true) — protected", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(1, [{ frameId: 0, candidates: [video(1, { drm: false })] }]);
    assert.deepEqual(await model.mainVideoOf(1), { frame_id: 0, document: "doc-top", element: 1 });
    await model.recordSurvey(2, [{ frameId: 0, candidates: [video(1, { drm: true, playing: true })] }]);
    assert.equal(await model.mainVideoOf(2), null);
    assert.equal((await model.videosFoundIn(2))[0]?.verdict, "protected");
  });

  it("форма not_supported_yet видна через модель вместе с формой источника", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.recordSurvey(6, [{ frameId: 0, candidates: [video(1, { media: { kind: "unsupported", form: "native_hls" }, playing: true })] }]);
    await model.noteHandoffRefused(6, { kind: "not_supported_yet", form: "native_hls" });
    assert.deepEqual(await model.lastRefusalOf(6), { kind: "not_supported_yet", form: "native_hls" });
    const [record] = await model.videosFoundIn(6);
    assert.deepEqual(record?.media, { kind: "unsupported", form: "native_hls" });
  });

  it("отказ вкладки, которую не опрашивали, сохраняется с пустым списком", async () => {
    const model = modelOver(createMemoryTabModelStore());
    await model.noteHandoffRefused(8, { kind: "tab_not_scriptable" });
    assert.deepEqual(await model.lastRefusalOf(8), { kind: "tab_not_scriptable" });
    assert.deepEqual(await model.videosFoundIn(8), []);
    assert.equal(await model.mainVideoOf(8), null);
  });
});

describe("засыпание фона и хранилище", () => {
  it("новая инстанция модели на том же хранилище видит снимок прежней", async () => {
    const store = createMemoryTabModelStore();
    const before = modelOver(store);
    await before.recordSurvey(7, AD_PREROLL);
    await before.noteHandoffRefused(7, { kind: "ad_playing" });
    const afterRestart = modelOver(store);
    assert.deepEqual(await afterRestart.mainVideoOf(7), { frame_id: 0, document: "doc-top", element: 1 });
    assert.equal(await afterRestart.isAdOn(7, { frame_id: 0, document: "doc-top", element: 2 }), true);
    assert.deepEqual(await afterRestart.lastRefusalOf(7), { kind: "ad_playing" });
  });

  it("forgetTab стирает снимок и в памяти, и в хранилище", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.forgetTab(7);
    assert.deepEqual(await model.videosFoundIn(7), []);
    assert.deepEqual(await modelOver(store).videosFoundIn(7), [], "после перезапуска фона тоже пусто");
    assert.equal(await store.get("tabModel:7"), undefined);
  });

  it("кривой снимок в хранилище считается отсутствующим, а не ломает модель", async () => {
    const store = createMemoryTabModelStore();
    await store.set("tabModel:7", { tab_id: 7, videos: "oops" });
    await store.set("tabModel:8", { tab_id: 9, surveyed_at_ms: 1, videos: [], videos_omitted: 0, main: null, last_refusal: null });
    const model = modelOver(store);
    assert.deepEqual(await model.videosFoundIn(7), []);
    assert.deepEqual(await model.videosFoundIn(8), [], "снимок чужой вкладки под этим ключом не принимается");
  });

  it("сбой хранилища не ломает модель и попадает в журнал один раз", async () => {
    const failures: unknown[] = [];
    const broken: TabModelStore = {
      get: () => Promise.reject(new Error("quota")),
      set: () => Promise.reject(new Error("quota")),
      remove: () => Promise.reject(new Error("quota")),
      keys: () => Promise.reject(new Error("quota")),
    };
    const model = modelOver(broken, failures);
    await model.recordSurvey(7, AD_PREROLL);
    assert.deepEqual(await model.mainVideoOf(7), { frame_id: 0, document: "doc-top", element: 1 }, "кэш в памяти работает");
    await model.noteHandoffRefused(7, { kind: "no_video" });
    await model.forgetTab(7);
    assert.deepEqual(await model.videosFoundIn(7), []);
    assert.equal(failures.length, 1, "сбой виден в журнале ровно один раз");
  });
});

describe("запись живёт только у открытых вкладок", () => {
  it("запоздавший отказ и опрос не воскрешают забытую вкладку: ни в хранилище, ни в памяти", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.forgetTab(7);
    await model.noteHandoffRefused(7, { kind: "page_flood" });
    await model.recordSurvey(7, AD_PREROLL);
    assert.deepEqual(await store.keys(), []);
    assert.equal(await model.lastRefusalOf(7), null);
    assert.deepEqual(await model.videosFoundIn(7), []);
  });

  it("отказ, ждавший чтения хранилища, не записывается, если вкладку забыли за это время", async () => {
    const memory = createMemoryTabModelStore();
    let releaseRead: () => void = () => {};
    const slowRead: TabModelStore = {
      ...memory,
      get: async (key) => {
        await new Promise<void>((resolve) => (releaseRead = resolve));
        return memory.get(key);
      },
    };
    const model = modelOver(slowRead);
    const refusal = model.noteHandoffRefused(7, { kind: "failed" });
    await model.forgetTab(7);
    releaseRead();
    await refusal;
    assert.deepEqual(await memory.keys(), []);
  });

  it("сбой чтения не затирает снимок: отказ не ложится поверх того, что мы не прочитали", async () => {
    const memory = createMemoryTabModelStore();
    await modelOver(memory).recordSurvey(7, AD_PREROLL);
    const failures: unknown[] = [];
    const unreadable: TabModelStore = { ...memory, get: () => Promise.reject(new Error("read failed")) };
    await modelOver(unreadable, failures).noteHandoffRefused(7, { kind: "page_flood" });
    assert.equal(failures.length, 1, "сбой виден");
    const reborn = modelOver(memory);
    assert.deepEqual(await reborn.mainVideoOf(7), { frame_id: 0, document: "doc-top", element: 1 }, "снимок цел");
  });

  it("onReplaced переносит снимок на новый номер и переживает сон фона", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.noteHandoffRefused(7, { kind: "ad_playing" });
    await model.moveTab(7, 9);
    assert.deepEqual(await store.keys(), ["tabModel:9"]);
    const reborn = modelOver(store);
    assert.deepEqual(await reborn.mainVideoOf(9), { frame_id: 0, document: "doc-top", element: 1 });
    assert.deepEqual(await reborn.lastRefusalOf(9), { kind: "ad_playing" });
    await model.noteHandoffRefused(7, { kind: "failed" });
    assert.deepEqual(await store.keys(), ["tabModel:9"], "старый номер не воскрес");
  });

  it("onReplaced: снимок лежал только в хранилище (фон проснулся) — переезжает так же", async () => {
    const store = createMemoryTabModelStore();
    await modelOver(store).recordSurvey(7, AD_PREROLL);
    await modelOver(store).moveTab(7, 9);
    assert.deepEqual(await store.keys(), ["tabModel:9"]);
  });

  it("onReplaced: свежий опрос новой вкладки не затирается старым снимком", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.recordSurvey(9, []);
    await model.moveTab(7, 9);
    assert.equal(await model.mainVideoOf(9), null);
    assert.deepEqual(await store.keys(), ["tabModel:9"]);
  });

  it("onReplaced без снимка ничего не создаёт", async () => {
    const store = createMemoryTabModelStore();
    await modelOver(store).moveTab(7, 9);
    assert.deepEqual(await store.keys(), []);
  });

  it("уборка на старте: записи закрытых вкладок стёрты, открытых и чужие ключи целы", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.recordSurvey(8, AD_PREROLL);
    await store.set("someOtherKey", 1);
    await modelOver(store).dropClosedTabs([8]);
    assert.deepEqual([...(await store.keys())].sort(), ["someOtherKey", "tabModel:8"]);
  });

  it("уборка не трогает снимок этой жизни: вкладка могла открыться после списка", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.dropClosedTabs([]);
    assert.deepEqual(await store.keys(), ["tabModel:7"]);
  });

  it("список ключей не прочитан: ничего не стирается, сбой виден", async () => {
    const memory = createMemoryTabModelStore();
    await modelOver(memory).recordSurvey(7, AD_PREROLL);
    const failures: unknown[] = [];
    await modelOver({ ...memory, keys: () => Promise.reject(new Error("read failed")) }, failures).dropClosedTabs([]);
    assert.deepEqual(await memory.keys(), ["tabModel:7"]);
    assert.equal(failures.length, 1);
  });
});

describe("жизнь вкладок браузера", () => {
  function fakeTabs(openTabs: Promise<readonly { readonly id?: number }[]>) {
    let replaced: (addedTabId: number, removedTabId: number) => void = () => {};
    const tabs: TabsLifecycleApi = {
      onReplaced: { addListener: (listener) => void (replaced = listener) },
      query: () => openTabs,
    };
    return { tabs, replace: (addedTabId: number, removedTabId: number) => replaced(addedTabId, removedTabId) };
  }

  it("старт фона: запись закрытой вкладки стёрта, открытой цела", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    await model.recordSurvey(8, AD_PREROLL);
    const { tabs } = fakeTabs(Promise.resolve([{ id: 8 }, {}]));
    await followTabLifecycle(modelOver(store), tabs, () => assert.fail("список получен"));
    assert.deepEqual(await store.keys(), ["tabModel:8"]);
  });

  it("список вкладок не получен: ничего не стёрто, сбой виден", async () => {
    const store = createMemoryTabModelStore();
    await modelOver(store).recordSurvey(7, AD_PREROLL);
    const failures: unknown[] = [];
    const { tabs } = fakeTabs(Promise.reject(new Error("no tabs api")));
    await followTabLifecycle(modelOver(store), tabs, (error) => void failures.push(error));
    assert.deepEqual(await store.keys(), ["tabModel:7"]);
    assert.equal(failures.length, 1);
  });

  it("tabs.onReplaced переносит запись: порядок аргументов браузера (новая, старая) соблюдён", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    await model.recordSurvey(7, AD_PREROLL);
    const { tabs, replace } = fakeTabs(Promise.resolve([{ id: 7 }]));
    await followTabLifecycle(model, tabs, () => {});
    replace(9, 7);
    await flushMicrotasks();
    assert.deepEqual(await store.keys(), ["tabModel:9"]);
  });
});

describe("хвост из 64 записей", () => {
  it("лишние отбрасываются, главное и играющие остаются, число отброшенных учтено", async () => {
    const store = createMemoryTabModelStore();
    const model = modelOver(store);
    const crowd: VideoCandidate[] = [];
    for (let element = 1; element <= 100; element += 1) {
      // 100 превью-плиток (junk), среди них одно играющее видео в самом конце.
      crowd.push(video(element, { muted: true, visible_area_px: 120 * 68 }));
    }
    crowd.push(video(101, { playing: true, visible_area_px: 640_000 }));
    await model.recordSurvey(9, [{ frameId: 0, candidates: crowd.slice(0, 100) }, { frameId: 1, candidates: crowd.slice(100) }]);
    const kept = await model.videosFoundIn(9);
    assert.equal(kept.length, MAX_TAB_VIDEO_RECORDS);
    assert.equal(kept[0]?.element, 101, "главное первое, хотя в опросе оно последнее");
    const stored = (await store.get("tabModel:9")) as { videos_omitted: number; videos: unknown[] };
    assert.equal(stored.videos.length + stored.videos_omitted, 101, "найдено = оставлено + отброшено");
    assert.equal(stored.videos_omitted, 101 - MAX_TAB_VIDEO_RECORDS);
    assert.deepEqual(await modelOver(store).mainVideoOf(9), { frame_id: 1, document: "doc-top", element: 101 });
  });

  it("главное (видимое на паузе) остаётся в хвосте, даже когда 100 скрытых играющих обгоняют его в порядке хвоста", async () => {
    const model = modelOver(createMemoryTabModelStore());
    const hidden = Array.from({ length: 100 }, (_, index) => video(index + 1, { media: { kind: "file" }, playing: true, visible_area_px: 0 }));
    await model.recordSurvey(10, [{ frameId: 0, candidates: [...hidden, video(500, { visible_area_px: 1000 })] }]);
    assert.deepEqual(await model.mainVideoOf(10), { frame_id: 0, document: "doc-top", element: 500 });
    assert.equal((await model.videosFoundIn(10))[0]?.element, 500, "запись главного не вытеснена из хвоста");
  });
});
