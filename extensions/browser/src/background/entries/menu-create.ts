// Создание одного пункта контекстного меню с честным ожиданием «создан».
//
// Корневая причина, ради которой модуль есть (живой прогон 10c, Firefox): `chrome.contextMenus.create(props, callback)`
// в фоне Firefox создаёт пункт, но колбэк НЕ вызывает. Цикл `installAll` ждал колбэк первого пункта вечно, и
// остальные два пункта (страница и ссылка) в Firefox не создавались вовсе: человек не видел ни пункта страницы, ни
// пункта ссылки. Chrome, наоборот, колбэк вызывает всегда и им сообщает об ошибке (`runtime.lastError`).
// Поэтому: в Chrome ждём колбэк и читаем `lastError`; в Firefox колбэк не передаём, а ошибку проверки аргументов
// (она бросается сразу) превращаем в отказ промиса.

/** Часть `contextMenus`, нужная для создания пункта. */
export interface MenuCreator {
  create(properties: Record<string, unknown>, callback?: () => void): unknown;
}

export interface CreateMenuItemOptions {
  /** Ошибка последнего вызова API (`chrome.runtime.lastError`); читается только внутри колбэка Chrome. */
  readonly lastError: () => { readonly message?: string | undefined } | undefined;
  /** `true` — браузер надёжно зовёт колбэк (Chrome); `false` — не зовёт (Firefox). */
  readonly callbackIsReliable: boolean;
}

export function createMenuItem(menus: MenuCreator, properties: Record<string, unknown>, options: CreateMenuItemOptions): Promise<void> {
  return new Promise((resolve, reject) => {
    try {
      if (!options.callbackIsReliable) {
        menus.create(properties);
        resolve();
        return;
      }
      menus.create(properties, () => {
        const failure = options.lastError();
        if (failure === undefined) resolve();
        else reject(new Error(failure.message ?? "contextMenus.create failed"));
      });
    } catch (error) {
      reject(error instanceof Error ? error : new Error(String(error)));
    }
  });
}
