// Настройки режима 2 «автозапуск» (сессия 12a): «Play videos in Fastiplayer when I press play».
// Поля общей настройки `autoHandoffSettings`: включатель и правило для сайта без своего правила.
// Фильтры видео с порогом длины — свой раздел (`video-filters-control.ts`, решения 260–261).
// Свои правила сайтов и свой стоп-лист живут в той же настройке, но редактируются на странице сессии 12b.
//
// Устроено как `entry-setting-control.ts`: изменение пишется сразу, без кнопки Save; перед записью настройка
// перечитывается из хранилища, и меняется только своё поле (другая вкладка настроек могла поменять соседние).
// Не прочли — не пишем (иначе затёрли бы чужие поля значениями по умолчанию). Ошибки показываем текстом.
import type { KeyValueStore } from "../shared/codec-storage.ts";
import {
  DEFAULT_AUTO_HANDOFF_SETTINGS,
  readAutoHandoffSettings,
  UNKNOWN_SITE_RULES,
  writeAutoHandoffSettings,
  type AutoHandoffSettings,
  type UnknownSiteRule,
} from "../shared/auto-handoff-settings.ts";

/** Подписи правила для сайта без своего правила (английский, пользовательский текст). */
const UNKNOWN_SITE_RULE_LABELS: Record<UnknownSiteRule, string> = {
  always: "Play in Fastiplayer",
  ask: "Ask first",
};

/** Изменить ровно поле «режим включён», остальные взять как есть. */
export function withAutoHandoffEnabled(current: AutoHandoffSettings, enabled: boolean): AutoHandoffSettings {
  return { ...current, enabled };
}

/** Изменить ровно правило для сайта без своего правила. */
export function withUnknownSiteRule(current: AutoHandoffSettings, rule: UnknownSiteRule): AutoHandoffSettings {
  return { ...current, unknown_site_rule: rule };
}

function unknownSiteRuleFrom(value: string): UnknownSiteRule | undefined {
  return UNKNOWN_SITE_RULES.find((rule) => rule === value);
}

/** Нарисовать поля в `container` и связать их с хранилищем. */
export async function mountAutoHandoffSettingControl(container: HTMLElement, store: KeyValueStore): Promise<void> {
  const heading = document.createElement("h2");
  heading.textContent = "Auto play";
  const status = document.createElement("p");
  status.id = "auto-handoff-status";

  const enabledBox = document.createElement("input");
  enabledBox.type = "checkbox";
  enabledBox.id = "auto-handoff-enabled";
  enabledBox.checked = DEFAULT_AUTO_HANDOFF_SETTINGS.enabled;
  const enabledText = document.createElement("span");
  enabledText.textContent = " Play videos in Fastiplayer when I press play";
  const enabledRow = document.createElement("label");
  enabledRow.append(enabledBox, enabledText);

  const ruleSelect = document.createElement("select");
  ruleSelect.id = "auto-handoff-unknown-site-rule";
  for (const rule of UNKNOWN_SITE_RULES) {
    const option = document.createElement("option");
    option.value = rule;
    option.textContent = UNKNOWN_SITE_RULE_LABELS[rule];
    ruleSelect.append(option);
  }
  ruleSelect.value = DEFAULT_AUTO_HANDOFF_SETTINGS.unknown_site_rule;
  const ruleText = document.createElement("span");
  ruleText.textContent = "On a site without its own rule: ";
  const ruleRow = document.createElement("label");
  ruleRow.append(ruleText, ruleSelect);

  for (const row of [enabledRow, ruleRow]) row.className = "setting-row";
  container.append(heading, enabledRow, ruleRow, status);

  /** Показать в полях то, что лежит в хранилище. */
  function showSettings(settings: AutoHandoffSettings): void {
    enabledBox.checked = settings.enabled;
    ruleSelect.value = settings.unknown_site_rule;
  }

  try {
    showSettings(await readAutoHandoffSettings(store));
  } catch (error) {
    status.textContent = `Could not read the settings: ${String(error)}`;
  }

  /** Перечитать, изменить своё поле, записать. Не прочли — не пишем. */
  function save(change: (current: AutoHandoffSettings) => AutoHandoffSettings): void {
    readAutoHandoffSettings(store)
      .then((current) => writeAutoHandoffSettings(store, change(current)))
      .then(
        () => (status.textContent = "Saved."),
        (error: unknown) => (status.textContent = `Could not save: ${String(error)}`),
      );
  }

  enabledBox.addEventListener("change", () => {
    const enabled = enabledBox.checked;
    save((current) => withAutoHandoffEnabled(current, enabled));
  });

  ruleSelect.addEventListener("change", () => {
    const rule = unknownSiteRuleFrom(ruleSelect.value);
    if (rule === undefined) return;
    save((current) => withUnknownSiteRule(current, rule));
  });
}
