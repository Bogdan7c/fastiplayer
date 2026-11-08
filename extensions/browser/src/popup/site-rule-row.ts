// Строка попапа «Auto play on <сайт>» (сессия 12b): правило автозапуска для сайта текущей вкладки.
// Рисует модель из `site-rule-row-model.ts`, при выборе сразу пишет настройку (чтение -> чистая правка ->
// запись только если что-то изменилось) и перерисовывается, когда настройку поменяли в другом окне.
// Строка живёт в своём корне и не трогает сценарий передачи видео (`popup-flow.ts`).

import { parseWebAddress } from "../background/auto-handoff/stop-list.ts";
import {
  readAutoHandoffSettings,
  writeAutoHandoffSettings,
  type AutoHandoffSettings,
} from "../shared/auto-handoff-settings.ts";
import { withSiteRuleChoice, type SettingsEditRejection, type SiteRuleChoice } from "../shared/auto-handoff-settings-edits.ts";
import type { KeyValueStore } from "../shared/codec-storage.ts";
import { buildSiteRuleRowModel, SITE_RULE_CHOICES, SITE_RULE_CHOICE_LABELS, type SiteRuleRowModel } from "./site-rule-row-model.ts";

/** Всё внешнее, что нужно строке (в тестах подменяется). */
export interface SiteRuleRowDependencies {
  /** Адрес активной вкладки, если браузер его отдаёт. */
  readonly findActiveTabUrl: () => Promise<string | undefined>;
  readonly store: KeyValueStore;
  /** Подписаться на новые значения настройки (попап живёт недолго, отписка не нужна). */
  readonly onSettingsChanged: (listener: (settings: AutoHandoffSettings) => void) => void;
}

/** Текст отказа правки (пользовательский, английский). */
export function describeRuleRejection(reason: SettingsEditRejection): string {
  switch (reason) {
    case "too_many_site_rules":
      return "Too many site rules. Remove some in the extension settings.";
    case "bad_site":
    case "bad_stop_list_entry":
    case "stop_list_full":
    case "filter_sites_full":
    case "bad_duration":
      return "This site address cannot be saved.";
  }
}

function textElement<Tag extends keyof HTMLElementTagNameMap>(tag: Tag, text: string, className?: string): HTMLElementTagNameMap[Tag] {
  const element = document.createElement(tag);
  element.textContent = text;
  if (className !== undefined) element.className = className;
  return element;
}

function choiceLabel(choice: SiteRuleChoice, model: SiteRuleRowModel): string {
  return choice === "default" ? model.defaultLabel : SITE_RULE_CHOICE_LABELS[choice];
}

/** Узнать вариант из значения `<select>`; чужое значение — `undefined`. */
function choiceFromValue(value: string): SiteRuleChoice | undefined {
  return SITE_RULE_CHOICES.find((choice) => choice === value);
}

/**
 * Показать строку в `root`. Адрес не http(s) или его нет — корень остаётся пустым. Сбой чтения настройки
 * показывается текстом `p.site-rule-status`, ничего не пишется.
 */
export async function mountSiteRuleRow(root: HTMLElement, dependencies: SiteRuleRowDependencies): Promise<void> {
  const tabUrl = await dependencies.findActiveTabUrl();
  // Не веб-страница (или адреса нет): строки нет вовсе, подписываться на настройку незачем.
  if (tabUrl === undefined || parseWebAddress(tabUrl) === undefined) return;

  const section = document.createElement("section");
  section.className = "site-rule";
  const status = textElement("p", "", "site-rule-status");
  /** Ключ из последней показанной модели: именно его правит выбор. */
  let shownRuleKey: string | undefined;

  function showError(prefix: string, error: unknown): void {
    status.textContent = `${prefix}: ${String(error)}`;
  }

  /** Сохранить выбор: перечитать настройку, применить правку, записать только при изменении. */
  async function saveChoice(ruleKey: string, choice: SiteRuleChoice): Promise<void> {
    try {
      const edit = withSiteRuleChoice(await readAutoHandoffSettings(dependencies.store), ruleKey, choice);
      if (edit.kind === "changed") await writeAutoHandoffSettings(dependencies.store, edit.settings);
      status.textContent = edit.kind === "rejected" ? describeRuleRejection(edit.reason) : "";
    } catch (error) {
      showError("Could not save", error);
    }
  }

  function render(settings: AutoHandoffSettings): void {
    const model = buildSiteRuleRowModel(tabUrl, settings);
    if (model === undefined) return;
    shownRuleKey = model.ruleKey;
    const select = document.createElement("select");
    select.className = "site-rule-choice";
    for (const choice of SITE_RULE_CHOICES) {
      const option = document.createElement("option");
      option.value = choice;
      option.textContent = choiceLabel(choice, model);
      select.append(option);
    }
    select.value = model.choice;
    select.addEventListener("change", () => {
      const choice = choiceFromValue(select.value);
      if (choice === undefined || shownRuleKey === undefined) return;
      void saveChoice(shownRuleKey, choice);
    });
    const label = document.createElement("label");
    label.append(textElement("span", `Auto play on ${model.ruleKey} `), select);
    const children: HTMLElement[] = [label];
    if (model.note !== undefined) children.push(textElement("p", model.note, "site-rule-note"));
    children.push(status);
    section.replaceChildren(...children);
  }

  root.replaceChildren(section);
  try {
    render(await readAutoHandoffSettings(dependencies.store));
  } catch (error) {
    showError("Could not read the settings", error);
    section.replaceChildren(status);
  }
  dependencies.onSettingsChanged(render);
}
