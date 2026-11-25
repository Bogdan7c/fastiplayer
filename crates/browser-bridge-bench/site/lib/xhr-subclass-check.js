// Самопроверка предусловия страниц с подменённым `XMLHttpRequest` (prelude стенда
// `site-extensions/bench-xhr-subclass-prelude.js`, подключается только в передаче и только
// строкой `page-subtitles`).

/**
 * Проверяет, что подмена стоит РАНЬШЕ скрипта экстеншена. Вне передачи (`pages`: чистый браузер
 * без экстеншена и без prelude) предусловия нет — проверка пропускается, страница ведёт себя как
 * обычная.
 */
export function checkXhrSubclassedBeforeTheExtension(context) {
  if (!context.params.handoff) return;
  const fact = window.__benchXhrSubclass;
  // Нативный прототип держит `status`; у прототипа подкласса своего геттера нет — как у блокировщика.
  const subclassHasOwnStatus = Object.getOwnPropertyDescriptor(XMLHttpRequest.prototype, 'status') !== undefined;
  const nativePrototypeHasStatus = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(XMLHttpRequest.prototype), 'status') !== undefined;
  context.check(
    'xhr-subclassed-before-the-extension',
    fact?.installed === true && fact.nativeOpenAtInstall === true && !subclassHasOwnStatus && nativePrototypeHasStatus,
    `installed=${fact?.installed} nativeOpenAtInstall=${fact?.nativeOpenAtInstall} subclassHasOwnStatus=${subclassHasOwnStatus} nativePrototypeHasStatus=${nativePrototypeHasStatus}`,
  );
}
