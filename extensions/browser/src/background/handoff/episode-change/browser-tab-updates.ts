// Настоящая подписка на новости о вкладках для сеансов передачи (сессия 16a): `tabs.onUpdated`.
//
// Почему именно `tabs.onUpdated`, а не `webNavigation.onHistoryStateUpdated`:
// - одно событие несёт и заголовок (`changeInfo.title`), и адрес главного документа (`changeInfo.url`):
//   заголовок нужен в любом случае, второй источник адреса лишь удвоил бы проводку;
// - `changeInfo.url` — «адрес вкладки, если он сменился» (документация Chrome и MDN), без оговорки о смене
//   документа; ОЖИДАЕМ, что он приходит и на `pushState`/`replaceState`, и на смену хеша. Документация этого
//   прямо не обещает ни для одного браузера — подтверждение живой страницей стенда (ниже). А
//   `onHistoryStateUpdated` смену хеша не видит точно (для неё по документации отдельное
//   `onReferenceFragmentUpdated`), а одностраничники с хеш-маршрутами есть;
// - вкладка — это всегда главный документ, отбирать `frameId === 0` не нужно;
// - новых прав не надо: `url` и `title` в `changeInfo` браузер даёт при правах на сайт вкладки
//   (`host_permissions: <all_urls>`; MDN: Chrome 50+, Firefox 86+) — на том же основании фон уже читает
//   заголовок через `tabs.get`. Право `tabs` по-прежнему не запрашивается;
// - фильтр у `addListener` не передаётся: его знает только Firefox, в Chrome у события один аргумент.
//
// Живая проверка — страница стенда `spa-navigation` в Chromium и Firefox. Если какой-то браузер на
// `pushState` не пришлёт `changeInfo.url`, второй источник (`webNavigation.onHistoryStateUpdated`,
// `frameId === 0`) подключается здесь же тем же `TabUpdate` — сеанс и правила не меняются (тогда нужна и
// правка `manifest/PERMISSIONS.md`: там сказано, что событий навигации экстеншен не слушает).
//
// Слушатель ставится не на верхнем уровне фона, а на время сеанса: будить уснувший service worker им не
// нужно — пока сеанс жив, его WebSocket держит фон бодрствующим.

import { browserApi } from "../../../shared/browser-api.ts";
import type { WatchTabUpdates } from "./session-episode-change.ts";

export const browserTabUpdates: WatchTabUpdates = (listener) => {
  const onUpdated = (tabId: number, changeInfo: chrome.tabs.OnUpdatedInfo): void => {
    // Смена значка, звука, закрепления и прочего сеансу не нужна.
    if (changeInfo.title === undefined && changeInfo.url === undefined) return;
    listener({ tabId, title: changeInfo.title, url: changeInfo.url });
  };
  browserApi.tabs.onUpdated.addListener(onUpdated);
  return () => browserApi.tabs.onUpdated.removeListener(onUpdated);
};
