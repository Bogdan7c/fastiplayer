// Следит за настройкой режима 2 «автозапуск» и говорит MAIN-миру, нужна ли охрана (сессия 12a, решение 198).
//
// Изолированный мир не знает адреса вкладки и правил сайтов (их знает только фон), поэтому решает грубо:
// `autoHandoffWatchOf` — «хоть где-то режим включён» и общие включатели фильтров звука и цикла (260–261). Точный вердикт для сайта выносит фон, когда страница
// донесёт о запуске зрителя. До этого фону не уходит ничего: страница и фон молчат.
//
// Порядок событий неочевиден (тот же случай, что в `overlay-setting-watch.ts`): чтение при старте
// асинхронное, а `storage.onChanged` может прийти раньше его ответа. Тогда побеждает новое значение из события,
// а запоздавший старый ответ чтения выбрасывается.
import { autoHandoffSettingsFromChange, autoHandoffWatchOf, type AutoHandoffSettings } from "../shared/auto-handoff-settings.ts";
import type { AutoHandoffWatch } from "../shared/auto-handoff-contract.ts";

export interface AutoHandoffWatchDependencies {
  /** Прочитать настройку (сбой хранилища уже даёт значения по умолчанию: режим выключен). */
  readonly readSettings: () => Promise<AutoHandoffSettings>;
  /** Подписка на изменения `storage.local`; возвращает отписку. */
  readonly onLocalStorageChanged: (listener: (changes: Readonly<Record<string, { readonly newValue?: unknown }>>) => void) => () => void;
  /** Сказать MAIN-миру (через пограничник), что охрана включена или выключена. */
  readonly applyWatch: (watch: AutoHandoffWatch) => void;
  readonly note: (message: string, error: unknown) => void;
}

function stateOf(settings: AutoHandoffSettings): AutoHandoffWatch {
  return autoHandoffWatchOf(settings);
}

/** Запустить слежение. Возвращает остановку (в продукте не зовётся: слежение живёт, пока жив документ). */
export function watchAutoHandoffSettings(dependencies: AutoHandoffWatchDependencies): () => void {
  let changeSeen = false;
  const stopListening = dependencies.onLocalStorageChanged((changes) => {
    const settings = autoHandoffSettingsFromChange(changes);
    if (settings === undefined) return; // изменился чужой ключ
    changeSeen = true;
    dependencies.applyWatch(stateOf(settings));
  });
  dependencies.readSettings().then(
    (settings) => {
      if (!changeSeen) dependencies.applyWatch(stateOf(settings));
    },
    // Не прочли — охрана остаётся выключенной (безопасное значение), а не угадывается.
    (error: unknown) => dependencies.note("could not read the auto handoff settings", error),
  );
  return stopListening;
}
