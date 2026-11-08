// Страница настроек: «Your stop list», «Your site rules», «Built-in stop list» (сессия 12b).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { BUILT_IN_STOP_LIST_GROUPS } from "../../src/background/auto-handoff/stop-list.ts";
import { mountAutoHandoffListsControl, NO_SITE_RULES_TEXT } from "../../src/options/auto-handoff-lists-control.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import { FakeElement, memoryStore, settingsChangeBus, storedSettings, withFakeDocument } from "./auto-handoff-lists-fake-dom.ts";
import { flushPromises } from "./support.ts";

const RICH: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "b.example": "ask", "a.example": "never" }, own_stop_list: ["x.example/shorts"] };

async function mount(initial: Record<string, unknown>) {
  const container = new FakeElement("div");
  const memory = memoryStore(initial);
  const bus = settingsChangeBus();
  await mountAutoHandoffListsControl(container as never, { store: memory.store, onSettingsChanged: bus.subscribe });
  return { container, bus, ...memory };
}

const status = (container: FakeElement) => container.byId("auto-handoff-lists-status").textContent;

async function addEntry(container: FakeElement, text: string): Promise<void> {
  container.byId("auto-handoff-stop-entry").value = text;
  container.byId("auto-handoff-stop-add").fire("click");
  await flushPromises();
}

describe("списки автозапуска на странице настроек", () => {
  it("показывает свой стоп-лист и правила по алфавиту", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      assert.deepEqual(container.byId("auto-handoff-own-stop-list").texts("span"), ["x.example/shorts "]);
      const rules = container.byId("auto-handoff-site-rules");
      assert.deepEqual(rules.texts("span"), ["a.example ", "b.example "]);
      assert.deepEqual(rules.select("select").map((select) => select.value), ["never", "ask"]);
      assert.deepEqual(rules.select("select")[0]?.children.map((option) => [option.value, option.textContent]), [["always", "Always"], ["never", "Never"], ["ask", "Ask first"]]);
    });
  });

  it("без правил показывает подсказку", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({});
      assert.deepEqual(container.byId("auto-handoff-site-rules").texts("li"), [NO_SITE_RULES_TEXT]);
    });
  });

  it("добавление записи: хранится нормализованная, поле очищается, список перерисован через onChanged", async () => {
    await withFakeDocument(async () => {
      const { container, values, bus } = await mount({});
      await addEntry(container, "https://WWW.Site.Example/Clips/");
      assert.deepEqual(storedSettings(values).own_stop_list, ["site.example/clips"]);
      assert.equal(container.byId("auto-handoff-stop-entry").value, "");
      assert.equal(status(container), "Saved.");
      bus.emit(storedSettings(values));
      assert.deepEqual(container.byId("auto-handoff-own-stop-list").texts("span"), ["site.example/clips "]);
    });
  });

  it("дубликат ничего не пишет", async () => {
    await withFakeDocument(async () => {
      const { container, state } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      await addEntry(container, "https://x.example/shorts/");
      assert.equal(state.writes, 0);
    });
  });

  it("неверная запись: статус «Not a site address», запись не выполнена, текст остаётся в поле", async () => {
    await withFakeDocument(async () => {
      const { container, state } = await mount({});
      await addEntry(container, "not a site");
      assert.equal(status(container), "Not a site address: not a site");
      assert.equal(state.writes, 0);
      assert.equal(container.byId("auto-handoff-stop-entry").value, "not a site");
    });
  });

  it("удаление записи стоп-листа", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      container.byId("auto-handoff-own-stop-list").one("button").fire("click");
      await flushPromises();
      assert.deepEqual(storedSettings(values).own_stop_list, []);
    });
  });

  it("смена правила в списке сохраняется", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      const first = container.byId("auto-handoff-site-rules").select("select")[0];
      assert.ok(first);
      first.value = "always";
      first.fire("change");
      await flushPromises();
      assert.deepEqual(storedSettings(values).site_rules, { "a.example": "always", "b.example": "ask" });
    });
  });

  it("Remove у правила снимает ключ", async () => {
    await withFakeDocument(async () => {
      const { container, values } = await mount({ [AUTO_HANDOFF_SETTINGS_KEY]: RICH });
      container.byId("auto-handoff-site-rules").select("button")[0]?.fire("click");
      await flushPromises();
      assert.deepEqual(storedSettings(values).site_rules, { "b.example": "ask" });
    });
  });

  it("сбой записи — текст ошибки; сбой чтения при открытии — сообщение", async () => {
    await withFakeDocument(async () => {
      const { container, state } = await mount({});
      state.failWrites = true;
      await addEntry(container, "ok.example");
      assert.match(status(container), /^Could not save: .*write broke/);
      const broken = new FakeElement("div");
      const memory = memoryStore();
      memory.state.failReads = true;
      await mountAutoHandoffListsControl(broken as never, { store: memory.store, onSettingsChanged: settingsChangeBus().subscribe });
      assert.match(status(broken), /^Could not read the settings: .*read broke/);
    });
  });

  it("встроенный стоп-лист: <details> со всеми группами и записями, только чтение", async () => {
    await withFakeDocument(async () => {
      const { container } = await mount({});
      const details = container.byId("auto-handoff-built-in-stop-list");
      assert.equal(details.tag, "details");
      assert.deepEqual(details.texts("h4"), BUILT_IN_STOP_LIST_GROUPS.map((group) => group.title));
      assert.deepEqual(details.texts("li"), BUILT_IN_STOP_LIST_GROUPS.flatMap((group) => group.entries));
      assert.equal(details.select("button").length, 0);
      assert.ok(details.texts("p").includes("Read only. Your rule Always for a site overrides this list."));
    });
  });

  it("правило, поставленное из попапа (onChanged), появляется в списке", async () => {
    await withFakeDocument(async () => {
      const { container, bus } = await mount({});
      bus.emit({ ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "new.example": "always" } });
      assert.deepEqual(container.byId("auto-handoff-site-rules").texts("span"), ["new.example "]);
    });
  });
});
