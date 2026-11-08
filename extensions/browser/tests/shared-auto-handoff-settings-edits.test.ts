// Чистые правки настройки автозапуска: правила сайтов, свой стоп-лист, фильтры видео (решения 260–261).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, MAX_OWN_STOP_LIST_ENTRIES, MAX_SITE_RULES, type AutoHandoffSettings } from "../src/shared/auto-handoff-settings.ts";
import {
  withMinDuration,
  withOwnStopListEntryAdded,
  withOwnStopListEntryRemoved,
  withSiteRuleChoice,
  withVideoFilterEnabled,
  withVideoFilterSiteAdded,
  withVideoFilterSiteRemoved,
  type SettingsEdit,
} from "../src/shared/auto-handoff-settings-edits.ts";
import { MAX_VIDEO_FILTER_SITES } from "../src/shared/video-filter-settings.ts";

const BASE: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "a.example": "never" }, own_stop_list: ["b.example/shorts"] };

describe("withSiteRuleChoice", () => {
  it("нормализует ключ: https не годится как хост, а www и регистр отбрасываются", () => {
    const edit = withSiteRuleChoice(BASE, "WWW.Video.Example.", "always");
    assert.deepEqual(edit, { kind: "changed", settings: { ...BASE, site_rules: { "a.example": "never", "video.example": "always" } } });
  });

  it("не хост (схема, путь, пусто) отклоняется как bad_site", () => {
    for (const bad of ["https://x.example", "x.example/path", "", "  "]) {
      assert.deepEqual(withSiteRuleChoice(BASE, bad, "always"), { kind: "rejected", reason: "bad_site" }, bad);
    }
  });

  it("смена правила заменяет значение, остальные правила не трогает", () => {
    const edit = withSiteRuleChoice(BASE, "a.example", "ask");
    assert.deepEqual(edit, { kind: "changed", settings: { ...BASE, site_rules: { "a.example": "ask" } } });
  });

  it("default снимает своё правило", () => {
    assert.deepEqual(withSiteRuleChoice(BASE, "a.example", "default"), { kind: "changed", settings: { ...BASE, site_rules: {} } });
  });

  it("то же правило и default без правила — unchanged", () => {
    assert.deepEqual(withSiteRuleChoice(BASE, "a.example", "never"), { kind: "unchanged" });
    assert.deepEqual(withSiteRuleChoice(BASE, "zzz.example", "default"), { kind: "unchanged" });
  });

  it("предел правил: новое сверх MAX_SITE_RULES отклоняется, смена существующего проходит", () => {
    const rules = Object.fromEntries(Array.from({ length: MAX_SITE_RULES }, (_, index) => [`s${index}.example`, "never" as const]));
    const full: AutoHandoffSettings = { ...BASE, site_rules: rules };
    assert.deepEqual(withSiteRuleChoice(full, "new.example", "always"), { kind: "rejected", reason: "too_many_site_rules" });
    assert.equal(withSiteRuleChoice(full, "s0.example", "always").kind, "changed");
  });

  it("исходная настройка не изменяется", () => {
    const before = structuredClone(BASE);
    withSiteRuleChoice(BASE, "a.example", "default");
    withSiteRuleChoice(BASE, "c.example", "always");
    assert.deepEqual(BASE, before);
  });
});

describe("свой стоп-лист", () => {
  it("запись нормализуется: схема, www, регистр, завершающий слэш", () => {
    const edit = withOwnStopListEntryAdded(BASE, "https://WWW.Site.Example/Clips/");
    assert.deepEqual(edit, { kind: "changed", settings: { ...BASE, own_stop_list: ["b.example/shorts", "site.example/clips"] } });
  });

  it("дубликат (в другой записи) — unchanged; мусор — bad_stop_list_entry", () => {
    assert.deepEqual(withOwnStopListEntryAdded(BASE, "https://www.B.example/shorts/"), { kind: "unchanged" });
    assert.deepEqual(withOwnStopListEntryAdded(BASE, "not a site"), { kind: "rejected", reason: "bad_stop_list_entry" });
  });

  it("полный список отклоняет новую запись", () => {
    const full: AutoHandoffSettings = { ...BASE, own_stop_list: Array.from({ length: MAX_OWN_STOP_LIST_ENTRIES }, (_, index) => `s${index}.example`) };
    assert.deepEqual(withOwnStopListEntryAdded(full, "new.example"), { kind: "rejected", reason: "stop_list_full" });
  });

  it("удаление принимает запись в любом виде; нет такой — unchanged", () => {
    assert.deepEqual(withOwnStopListEntryRemoved(BASE, "https://b.example/shorts/"), { kind: "changed", settings: { ...BASE, own_stop_list: [] } });
    assert.deepEqual(withOwnStopListEntryRemoved(BASE, "other.example"), { kind: "unchanged" });
    assert.deepEqual(withOwnStopListEntryRemoved(BASE, "???"), { kind: "unchanged" });
  });

  it("исходная настройка не изменяется", () => {
    const before = structuredClone(BASE);
    withOwnStopListEntryAdded(BASE, "c.example");
    withOwnStopListEntryRemoved(BASE, "b.example/shorts");
    assert.deepEqual(BASE, before);
  });
});

/** Настройка из итога `changed` (иначе тест падает). */
function changedSettings(edit: SettingsEdit): AutoHandoffSettings {
  assert.equal(edit.kind, "changed");
  return (edit as { readonly settings: AutoHandoffSettings }).settings;
}

describe("правки фильтров видео", () => {
  it("включатель: меняет только свой фильтр; то же значение — unchanged", () => {
    const next = changedSettings(withVideoFilterEnabled(BASE, "loop", true));
    assert.deepEqual(next.video_filters.loop, { enabled: true, sites: [] });
    assert.deepEqual(next.video_filters.sound, BASE.video_filters.sound);
    assert.deepEqual(next.site_rules, BASE.site_rules);
    assert.deepEqual(withVideoFilterEnabled(BASE, "loop", false), { kind: "unchanged" });
  });

  it("сайт: адрес целиком приводится к ключу; повтор — unchanged; не адрес — bad_site", () => {
    const next = changedSettings(withVideoFilterSiteAdded(BASE, "sound", " HTTPS://www.Feed.example/reel/1 "));
    assert.deepEqual(next.video_filters.sound, { enabled: false, sites: ["feed.example"] });
    assert.deepEqual(withVideoFilterSiteAdded(next, "sound", "feed.example"), { kind: "unchanged" });
    for (const bad of ["", "a b", "https://", "exa_mple.com"]) assert.deepEqual(withVideoFilterSiteAdded(BASE, "sound", bad), { kind: "rejected", reason: "bad_site" }, bad);
  });

  it("сайт: полный список — filter_sites_full; удаление — только свой сайт, нет такого — unchanged", () => {
    const sites = Array.from({ length: MAX_VIDEO_FILTER_SITES }, (_, index) => `s${index}.example`);
    const full: AutoHandoffSettings = { ...BASE, video_filters: { ...BASE.video_filters, duration: { enabled: true, sites } } };
    assert.deepEqual(withVideoFilterSiteAdded(full, "duration", "new.example"), { kind: "rejected", reason: "filter_sites_full" });
    const removed = changedSettings(withVideoFilterSiteRemoved(full, "duration", "www.s0.example"));
    assert.equal(removed.video_filters.duration.sites.includes("s0.example"), false);
    assert.equal(removed.video_filters.duration.enabled, true, "включатель не тронут");
    assert.deepEqual(withVideoFilterSiteRemoved(BASE, "duration", "none.example"), { kind: "unchanged" });
  });

  it("порог: целое в пределах пишется; дробь, минус и сверх предела — bad_duration", () => {
    assert.equal(changedSettings(withMinDuration(BASE, 0)).min_duration_s, 0);
    assert.deepEqual(withMinDuration(BASE, BASE.min_duration_s), { kind: "unchanged" });
    for (const bad of [-1, 1.5, Number.NaN, 3_601]) assert.deepEqual(withMinDuration(BASE, bad), { kind: "rejected", reason: "bad_duration" }, String(bad));
  });

  it("правки не меняют исходную настройку", () => {
    const before = structuredClone(BASE);
    withVideoFilterEnabled(BASE, "sound", true);
    withVideoFilterSiteAdded(BASE, "sound", "a.example");
    withMinDuration(BASE, 5);
    assert.deepEqual(BASE, before);
  });
});
