// Чистые правки настройки автозапуска: правила сайтов и свой стоп-лист.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, MAX_OWN_STOP_LIST_ENTRIES, MAX_SITE_RULES, type AutoHandoffSettings } from "../src/shared/auto-handoff-settings.ts";
import { withOwnStopListEntryAdded, withOwnStopListEntryRemoved, withSiteRuleChoice } from "../src/shared/auto-handoff-settings-edits.ts";

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
