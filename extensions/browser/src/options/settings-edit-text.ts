// Тексты отказов правок настройки для страницы настроек (английский, пользовательский текст).
import type { SettingsEditRejection } from "../shared/auto-handoff-settings-edits.ts";

/** Текст статуса для отказа правки; `entry` — то, что ввёл человек (для «Not a site address»). */
export function describeEditRejection(reason: SettingsEditRejection, entry: string): string {
  switch (reason) {
    case "bad_stop_list_entry":
    case "bad_site":
      return `Not a site address: ${entry}`;
    case "stop_list_full":
      return "The stop list is full. Remove an entry first.";
    case "too_many_site_rules":
      return "Too many site rules. Remove some first.";
  }
}
