// Страница настроек: раздел «Video filters» (решения 260–261) — галочка «везде», порог длины и свой
// список сайтов у каждого фильтра. Проверяем то, что видит человек, и то, что легло в хранилище.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { mountVideoFiltersControl, NO_FILTER_SITES_TEXT, parseMinDurationInput, VIDEO_FILTER_TITLES } from "../../src/options/video-filters-control.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import { MAX_VIDEO_FILTER_SITES } from "../../src/shared/video-filter-settings.ts";
import { FakeElement, memoryStore, settingsChangeBus, storedSettings, withFakeDocument } from "./auto-handoff-lists-fake-dom.ts";
import { flushPromises } from "./support.ts";

async function mount(initial: Record<string, unknown> = {}) {
  const container = new FakeElement("div");
  const memory = memoryStore(initial);
  const bus = settingsChangeBus();
  await mountVideoFiltersControl(container as never, { store: memory.store, onSettingsChanged: bus.subscribe });
  return { container, bus, ...memory };
}

const status = (container: FakeElement): string => container.byId("video-filters-status").textContent;

async function addSite(container: FakeElement, filter: string, text: string): Promise<void> {
  container.byId(`video-filter-${filter}-site`).value = text;
  container.byId(`video-filter-${filter}-add`).fire("click");
  await flushPromises();
}

describe("раздел «Video filters»", () => {
  it("по умолчанию все три фильтра выключены, списки пусты, порог 60", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount();
      assert.deepEqual(container.texts("h3"), [VIDEO_FILTER_TITLES.sound, VIDEO_FILTER_TITLES.duration, VIDEO_FILTER_TITLES.loop]);
      for (const name of ["sound", "duration", "loop"]) {
        assert.equal(container.byId(`video-filter-${name}-enabled`).checked, false, name);
        assert.deepEqual(container.byId(`video-filter-${name}-sites`).texts("li"), [NO_FILTER_SITES_TEXT], name);
      }
      assert.equal(container.byId("video-filter-duration-seconds").value, "60");
    });
  });

  it("галочка «On every site» пишет только свой фильтр, остальная настройка цела", async () => {
    await withFakeDocument(async () => {
      const rich: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true, site_rules: { "a.example": "ask" } };
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: rich });
      const loopBox = container.byId("video-filter-loop-enabled");
      loopBox.checked = true;
      loopBox.fire("change");
      await flushPromises();
      const stored = storedSettings(values);
      assert.deepEqual(stored.video_filters.loop, { enabled: true, sites: [] });
      assert.deepEqual(stored.video_filters.sound, DEFAULT_AUTO_HANDOFF_SETTINGS.video_filters.sound);
      assert.equal(stored.enabled, true);
      assert.deepEqual(stored.site_rules, { "a.example": "ask" });
      assert.equal(status(container), "Saved.");
    });
  });

  it("сайт в список: адрес вставлен целиком — хранится ключ сайта, поле очищено, список перерисован", async () => {
    await withFakeDocument(async () => {
      const { container, values, bus } = await mount();
      await addSite(container, "sound", "https://WWW.Feed.example/reel/123?x=1");
      assert.deepEqual(storedSettings(values).video_filters.sound, { enabled: false, sites: ["feed.example"] });
      assert.equal(container.byId("video-filter-sound-site").value, "");
      bus.emit(storedSettings(values));
      assert.deepEqual(container.byId("video-filter-sound-sites").texts("span"), ["feed.example "]);
      assert.deepEqual(container.byId("video-filter-duration-sites").texts("li"), [NO_FILTER_SITES_TEXT], "у других фильтров списки свои");
    });
  });

  it("неверный адрес не пишется: статус «Not a site address», текст остаётся в поле", async () => {
    await withFakeDocument(async () => {
      const { container, state } = await mount();
      await addSite(container, "loop", "not a site");
      assert.equal(state.writes, 0);
      assert.match(status(container), /Not a site address: not a site/);
      assert.equal(container.byId("video-filter-loop-site").value, "not a site");
    });
  });

  it("повтор ничего не пишет; полный список — отказ с понятным текстом", async () => {
    await withFakeDocument(async () => {
      const sites = Array.from({ length: MAX_VIDEO_FILTER_SITES }, (_, index) => `s${index}.example`);
      const full = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, video_filters: { ...DEFAULT_AUTO_HANDOFF_SETTINGS.video_filters, duration: { enabled: false, sites } } };
      const { container, state } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: full });
      await addSite(container, "duration", "s1.example");
      assert.equal(state.writes, 0);
      assert.equal(status(container), "Saved.");
      await addSite(container, "duration", "new.example");
      assert.equal(state.writes, 0);
      assert.match(status(container), /site list is full/);
    });
  });

  it("Remove убирает сайт из списка своего фильтра", async () => {
    await withFakeDocument(async () => {
      const start = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, video_filters: { ...DEFAULT_AUTO_HANDOFF_SETTINGS.video_filters, loop: { enabled: true, sites: ["a.example", "b.example"] } } };
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: start });
      const removeButtons = container.byId("video-filter-loop-sites").select("button");
      removeButtons[0]?.fire("click");
      await flushPromises();
      assert.deepEqual(storedSettings(values).video_filters.loop, { enabled: true, sites: ["b.example"] });
    });
  });

  it("порог: верный пишется; неверный не пишется, поле возвращается к хранимому, видна подсказка", async () => {
    await withFakeDocument(async () => {
      const { container, values, state } = await mount();
      const seconds = container.byId("video-filter-duration-seconds");
      seconds.value = "90";
      seconds.fire("change");
      await flushPromises();
      assert.equal(storedSettings(values).min_duration_s, 90);
      const writes = state.writes;
      for (const bad of ["", "abc", "-5", "1.5", "99999", "3601"]) {
        seconds.value = bad;
        seconds.fire("change");
        await flushPromises();
        assert.equal(state.writes, writes, bad);
        assert.equal(seconds.value, "90", bad);
        assert.match(status(container), /whole number of seconds/, bad);
      }
    });
  });

  it("сбой записи показывается текстом, а не глотается", async () => {
    await withFakeDocument(async () => {
      const { container, state } = await mount();
      state.failWrites = true;
      const box = container.byId("video-filter-sound-enabled");
      box.checked = true;
      box.fire("change");
      await flushPromises();
      assert.match(status(container), /Could not save: .*write broke/);
    });
  });

  it("parseMinDurationInput: границы", () => {
    assert.equal(parseMinDurationInput("0"), 0);
    assert.equal(parseMinDurationInput(" 3600 "), 3600);
    assert.equal(parseMinDurationInput("3601"), undefined);
    assert.equal(parseMinDurationInput("1e3"), undefined);
  });
});
