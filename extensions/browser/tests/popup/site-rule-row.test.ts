// Строка попапа «Auto play on <сайт>»: что нарисовано, что записано после выбора, что при сбоях.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { mountSiteRuleRow } from "../../src/popup/site-rule-row.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import { flushPromises } from "../background/support.ts";
import { FakeElement, memoryStore, settingsChangeBus, storedSettings, withFakeDocument } from "../background/auto-handoff-lists-fake-dom.ts";

const WITH_RULE: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true, site_rules: { "video.example": "never" } };

async function mount(url: string | undefined, initial: Record<string, unknown>) {
  const root = new FakeElement("div");
  const memory = memoryStore(initial);
  const bus = settingsChangeBus();
  await mountSiteRuleRow(root as never, { findActiveTabUrl: async () => url, store: memory.store, onSettingsChanged: bus.subscribe });
  return { root, bus, ...memory };
}

const choose = async (root: FakeElement, value: string) => {
  root.one("select", "site-rule-choice").value = value;
  root.one("select", "site-rule-choice").fire("change");
  await flushPromises();
};

describe("строка правила сайта в попапе", () => {
  it("без адреса или не на веб-странице корень пуст", async () => {
    await withFakeDocument(async () => {
      for (const url of [undefined, "chrome://extensions"]) {
        const { root } = await mount(url, {});
        assert.deepEqual(root.children, [], String(url));
      }
    });
  });

  it("рисует section.site-rule с вариантами default|always|never|ask и текущим выбором", async () => {
    await withFakeDocument(async () => {
      const { root } = await mount("https://sub.video.example/x", { [AUTO_HANDOFF_SETTINGS_KEY]: WITH_RULE });
      const section = root.one("section", "site-rule");
      const select = section.one("select", "site-rule-choice");
      assert.deepEqual(select.children.map((option) => option.value), ["default", "always", "never", "ask"]);
      assert.deepEqual(select.children.map((option) => option.textContent), ["Default (always)", "Always", "Never", "Ask first"]);
      assert.equal(select.value, "never");
      assert.deepEqual(section.texts("span"), ["Auto play on video.example "]);
    });
  });

  it("выбор Always пишет правило на точный ключ сайта, остальное не трогает", async () => {
    await withFakeDocument(async () => {
      const { root, values } = await mount("https://www.new.example/", { [AUTO_HANDOFF_SETTINGS_KEY]: WITH_RULE });
      await choose(root, "always");
      assert.deepEqual(storedSettings(values), { ...WITH_RULE, site_rules: { "video.example": "never", "new.example": "always" } });
      assert.equal(root.one("p", "site-rule-status").textContent, "");
    });
  });

  it("выбор Default снимает правило, найденное на родителе", async () => {
    await withFakeDocument(async () => {
      const { root, values } = await mount("https://sub.video.example/", { [AUTO_HANDOFF_SETTINGS_KEY]: WITH_RULE });
      await choose(root, "default");
      assert.deepEqual(storedSettings(values).site_rules, {});
    });
  });

  it("тот же выбор ничего не пишет", async () => {
    await withFakeDocument(async () => {
      const { root, state } = await mount("https://video.example/", { [AUTO_HANDOFF_SETTINGS_KEY]: WITH_RULE });
      await choose(root, "never");
      assert.equal(state.writes, 0);
    });
  });

  it("заметки про стоп-лист показываются", async () => {
    await withFakeDocument(async () => {
      const { root } = await mount("https://meet.google.com/x", {});
      assert.match(root.one("p", "site-rule-note").textContent, /^Built-in stop list/);
    });
  });

  it("сбой чтения при выборе: текст ошибки, запись не выполнена", async () => {
    await withFakeDocument(async () => {
      const { root, state } = await mount("https://video.example/", {});
      state.failReads = true;
      await choose(root, "always");
      assert.match(root.one("p", "site-rule-status").textContent, /^Could not save: .*read broke/);
      assert.equal(state.writes, 0);
    });
  });

  it("сбой записи показывается текстом", async () => {
    await withFakeDocument(async () => {
      const { root, state } = await mount("https://video.example/", {});
      state.failWrites = true;
      await choose(root, "always");
      assert.match(root.one("p", "site-rule-status").textContent, /^Could not save: .*write broke/);
    });
  });

  it("сбой чтения при открытии: только сообщение, выбора нет", async () => {
    await withFakeDocument(async () => {
      const root = new FakeElement("div");
      const memory = memoryStore();
      memory.state.failReads = true;
      await mountSiteRuleRow(root as never, { findActiveTabUrl: async () => "https://a.example/", store: memory.store, onSettingsChanged: settingsChangeBus().subscribe });
      assert.equal(root.select("select").length, 0);
      assert.match(root.one("p", "site-rule-status").textContent, /^Could not read the settings: .*read broke/);
    });
  });

  it("перерисовывается, когда настройку меняют снаружи", async () => {
    await withFakeDocument(async () => {
      const { root, bus } = await mount("https://video.example/", {});
      assert.equal(root.one("select", "site-rule-choice").value, "default");
      bus.emit({ ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true, unknown_site_rule: "ask", site_rules: { "video.example": "ask" } });
      const select = root.one("select", "site-rule-choice");
      assert.equal(select.value, "ask");
      assert.equal(select.children[0]?.textContent, "Default (ask first)");
    });
  });
});
