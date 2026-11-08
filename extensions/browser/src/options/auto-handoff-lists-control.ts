// Страница настроек, сессия 12b: «Your stop list», «Your site rules» и «Built-in stop list» (только чтение).
// Стиль `auto-handoff-setting-control.ts`: createElement, правка сохраняется сразу, статус «Saved.» /
// «Could not save: …». Любая запись: прочитать настройку -> чистая правка (`auto-handoff-settings-edits.ts`)
// -> писать только при `changed`. Списки перерисовываются, когда настройку меняют в другом окне
// (попап и предложение на видео тоже пишут правила сайтов).

import { BUILT_IN_STOP_LIST_GROUPS } from "../background/auto-handoff/stop-list.ts";
import {
  readAutoHandoffSettings,
  SITE_RULES,
  writeAutoHandoffSettings,
  type AutoHandoffSettings,
  type SiteRule,
} from "../shared/auto-handoff-settings.ts";
import {
  withOwnStopListEntryAdded,
  withOwnStopListEntryRemoved,
  withSiteRuleChoice,
  type SettingsEdit,
  type SiteRuleChoice,
} from "../shared/auto-handoff-settings-edits.ts";
import type { KeyValueStore } from "../shared/codec-storage.ts";
import { describeEditRejection } from "./settings-edit-text.ts";

/** Подписи правил в списке (английский, пользовательский текст). */
const SITE_RULE_LABELS: Record<SiteRule, string> = { always: "Always", never: "Never", ask: "Ask first" };

/** Подсказка, когда правил нет. */
export const NO_SITE_RULES_TEXT = "No site rules yet. Set one from the toolbar popup or the offer on a video.";

/** Что нужно контролу снаружи (в тестах подменяется). */
export interface AutoHandoffListsDependencies {
  readonly store: KeyValueStore;
  /** Подписаться на новые значения настройки. */
  readonly onSettingsChanged: (listener: (settings: AutoHandoffSettings) => void) => void;
}

/** Правила сайтов по порядку ключей (для стабильного списка). */
export function sortedSiteRules(settings: AutoHandoffSettings): readonly (readonly [string, SiteRule])[] {
  return Object.entries(settings.site_rules).sort(([first], [second]) => first.localeCompare(second));
}

function ruleFromValue(value: string): SiteRule | undefined {
  return SITE_RULES.find((rule) => rule === value);
}

function textElement<Tag extends keyof HTMLElementTagNameMap>(tag: Tag, text: string): HTMLElementTagNameMap[Tag] {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
}

function buttonElement(text: string, id?: string): HTMLButtonElement {
  const button = textElement("button", text);
  button.type = "button";
  if (id !== undefined) button.id = id;
  return button;
}

/** Нарисовать три раздела в `container` и связать их с хранилищем. */
export async function mountAutoHandoffListsControl(container: HTMLElement, dependencies: AutoHandoffListsDependencies): Promise<void> {
  const status = document.createElement("p");
  status.id = "auto-handoff-lists-status";

  // Свой стоп-лист.
  const stopInput = document.createElement("input");
  stopInput.type = "text";
  stopInput.id = "auto-handoff-stop-entry";
  stopInput.placeholder = "example.com or example.com/section";
  const stopAdd = buttonElement("Add", "auto-handoff-stop-add");
  const stopList = document.createElement("ul");
  stopList.id = "auto-handoff-own-stop-list";
  const stopRow = document.createElement("div");
  stopRow.className = "setting-row";
  stopRow.append(stopInput, stopAdd);

  // Свои правила сайтов.
  const rulesList = document.createElement("ul");
  rulesList.id = "auto-handoff-site-rules";

  // Встроенный стоп-лист: только чтение, внутри <details>.
  const builtIn = document.createElement("details");
  builtIn.id = "auto-handoff-built-in-stop-list";
  builtIn.append(textElement("summary", "Built-in stop list"), textElement("p", "Read only. Your rule Always for a site overrides this list."));
  for (const group of BUILT_IN_STOP_LIST_GROUPS) {
    const entries = document.createElement("ul");
    for (const entry of group.entries) entries.append(textElement("li", entry));
    builtIn.append(textElement("h4", group.title), entries);
  }

  container.append(
    textElement("h3", "Your stop list"),
    textElement("p", "Auto play never starts on these sites. Use example.com or example.com/section."),
    stopRow,
    stopList,
    textElement("h3", "Your site rules"),
    rulesList,
    builtIn,
    status,
  );

  /** Применить правку: читаем свежую настройку, пишем только при `changed`. Возвращает `true`, если правка принята. */
  async function applyEdit(edit: (current: AutoHandoffSettings) => SettingsEdit, rejectedEntry: string): Promise<boolean> {
    try {
      const result = edit(await readAutoHandoffSettings(dependencies.store));
      if (result.kind === "rejected") {
        status.textContent = describeEditRejection(result.reason, rejectedEntry);
        return false;
      }
      if (result.kind === "changed") await writeAutoHandoffSettings(dependencies.store, result.settings);
      status.textContent = "Saved.";
      return true;
    } catch (error) {
      status.textContent = `Could not save: ${String(error)}`;
      return false;
    }
  }

  function renderStopList(settings: AutoHandoffSettings): void {
    const items = settings.own_stop_list.map((entry) => {
      const remove = buttonElement("Remove");
      remove.addEventListener("click", () => void applyEdit((current) => withOwnStopListEntryRemoved(current, entry), entry));
      const item = document.createElement("li");
      item.append(textElement("span", `${entry} `), remove);
      return item;
    });
    stopList.replaceChildren(...items);
  }

  function renderSiteRules(settings: AutoHandoffSettings): void {
    const rules = sortedSiteRules(settings);
    if (rules.length === 0) {
      rulesList.replaceChildren(textElement("li", NO_SITE_RULES_TEXT));
      return;
    }
    const items = rules.map(([siteKey, rule]) => {
      const select = document.createElement("select");
      for (const choice of SITE_RULES) {
        const option = document.createElement("option");
        option.value = choice;
        option.textContent = SITE_RULE_LABELS[choice];
        select.append(option);
      }
      select.value = rule;
      select.addEventListener("change", () => {
        const choice = ruleFromValue(select.value);
        if (choice !== undefined) void applyEdit((current) => withSiteRuleChoice(current, siteKey, choice), siteKey);
      });
      const remove = buttonElement("Remove");
      const defaultChoice: SiteRuleChoice = "default";
      remove.addEventListener("click", () => void applyEdit((current) => withSiteRuleChoice(current, siteKey, defaultChoice), siteKey));
      const item = document.createElement("li");
      item.append(textElement("span", `${siteKey} `), select, remove);
      return item;
    });
    rulesList.replaceChildren(...items);
  }

  function renderLists(settings: AutoHandoffSettings): void {
    renderStopList(settings);
    renderSiteRules(settings);
  }

  stopAdd.addEventListener("click", () => {
    const entry = stopInput.value;
    void applyEdit((current) => withOwnStopListEntryAdded(current, entry), entry).then((accepted) => {
      if (accepted) stopInput.value = "";
    });
  });

  try {
    renderLists(await readAutoHandoffSettings(dependencies.store));
  } catch (error) {
    status.textContent = `Could not read the settings: ${String(error)}`;
  }
  dependencies.onSettingsChanged(renderLists);
}
