// Следит за настройкой «Button over videos» и включает/выключает кнопку на лету.
//
// Порядок событий неочевиден: чтение при старте асинхронное, а `storage.onChanged` может прийти раньше его
// ответа (человек щёлкнул чекбокс сразу после загрузки страницы). Тогда побеждает новое значение из события,
// а запоздавший старый ответ чтения выбрасывается.
import { entrySettingsFromChange, type EntrySettings } from "../../shared/entry-settings.ts";

export interface OverlaySettingWatchDependencies {
  /** Прочитать настройку из `storage.local` (разбор мусора уже внутри). Может отклониться. */
  readonly readSettings: () => Promise<EntrySettings>;
  /** Подписка на изменения `storage.local`; возвращает отписку. Колбэк получает «сырой» объект изменений. */
  readonly onLocalStorageChanged: (listener: (changes: Readonly<Record<string, { readonly newValue?: unknown }>>) => void) => () => void;
  /** Применить значение к кнопке. */
  readonly applyEnabled: (enabled: boolean) => void;
  readonly note: (message: string, error: unknown) => void;
}

/** Запустить слежение. Возвращает остановку (в продукте не зовётся: слежение живёт, пока жив документ). */
export function watchOverlaySetting(dependencies: OverlaySettingWatchDependencies): () => void {
  let changeSeen = false;
  const stopListening = dependencies.onLocalStorageChanged((changes) => {
    const settings = entrySettingsFromChange(changes);
    if (settings === undefined) return; // изменился чужой ключ
    changeSeen = true;
    dependencies.applyEnabled(settings.overlay_button);
  });
  dependencies.readSettings().then(
    (settings) => {
      if (!changeSeen) dependencies.applyEnabled(settings.overlay_button);
    },
    // Не прочли — кнопка остаётся спящей (безопасное значение), а не угадывается.
    (error: unknown) => dependencies.note("could not read the entry settings", error),
  );
  return stopListening;
}
