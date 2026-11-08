// Модель строки «правило сайта» в попапе: ключ, подпись «по умолчанию», заметки.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { buildSiteRuleRowModel, BUILT_IN_STOP_LIST_NOTE, OWN_STOP_LIST_NOTE } from "../../src/popup/site-rule-row-model.ts";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";

const OFF = DEFAULT_AUTO_HANDOFF_SETTINGS;
const settings = (patch: Partial<AutoHandoffSettings>): AutoHandoffSettings => ({ ...OFF, ...patch });

describe("модель строки правила сайта", () => {
  it("не веб-страница или нет адреса — строки нет", () => {
    for (const url of [undefined, "", "chrome://extensions", "file:///a.mp4", "about:blank"]) {
      assert.equal(buildSiteRuleRowModel(url, OFF), undefined, String(url));
    }
  });

  it("без своего правила правится точный ключ сайта (www отбрасывается), выбор — default", () => {
    const model = buildSiteRuleRowModel("https://www.Video.example/watch?v=1", OFF);
    assert.equal(model?.ruleKey, "video.example");
    assert.equal(model?.choice, "default");
  });

  it("правило на родителе: правится ключ родителя, выбор — его правило", () => {
    const model = buildSiteRuleRowModel("https://sub.video.example/x", settings({ site_rules: { "video.example": "never" } }));
    assert.equal(model?.ruleKey, "video.example");
    assert.equal(model?.choice, "never");
  });

  it("точное правило сильнее родительского", () => {
    const model = buildSiteRuleRowModel("https://sub.video.example/", settings({ site_rules: { "video.example": "never", "sub.video.example": "ask" } }));
    assert.deepEqual([model?.ruleKey, model?.choice], ["sub.video.example", "ask"]);
  });

  it("подпись default зависит от общей настройки", () => {
    const label = (patch: Partial<AutoHandoffSettings>) => buildSiteRuleRowModel("https://a.example/", settings(patch))?.defaultLabel;
    assert.equal(label({ enabled: false, unknown_site_rule: "always" }), "Default (off)");
    assert.equal(label({ enabled: true, unknown_site_rule: "always" }), "Default (always)");
    assert.equal(label({ enabled: true, unknown_site_rule: "ask" }), "Default (ask first)");
  });

  it("заметка про свой стоп-лист — при любом правиле, включая always", () => {
    const stop = { own_stop_list: ["a.example/shorts"], site_rules: { "a.example": "always" as const } };
    assert.equal(buildSiteRuleRowModel("https://a.example/shorts/1", settings(stop))?.note, OWN_STOP_LIST_NOTE);
    assert.equal(buildSiteRuleRowModel("https://a.example/watch", settings(stop))?.note, undefined);
  });

  it("заметка про встроенный стоп-лист: есть, пока правило не always", () => {
    const url = "https://meet.google.com/abc";
    assert.equal(buildSiteRuleRowModel(url, OFF)?.note, BUILT_IN_STOP_LIST_NOTE);
    assert.equal(buildSiteRuleRowModel(url, settings({ site_rules: { "meet.google.com": "never" } }))?.note, BUILT_IN_STOP_LIST_NOTE);
    assert.equal(buildSiteRuleRowModel(url, settings({ site_rules: { "meet.google.com": "always" } }))?.note, undefined);
  });
});
