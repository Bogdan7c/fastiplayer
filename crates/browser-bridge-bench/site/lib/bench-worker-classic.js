// Классический Dedicated Worker стенда, как у Twitch (IVS): страница создаёт воркер из своего
// blob-адреса с `importScripts('<адрес этого файла>')` (bench-player.js), без import на верхнем
// уровне. Плеер (модуль bench-worker-core.js) подгружается динамическим `import()` по АБСОЛЮТНОМУ
// адресу из первого сообщения страницы (`coreUrl`): `self.location` здесь — blob-адрес страницы,
// и относительный адрес от него не разрешается (как у настоящих blob-воркеров, с экстеншеном и без).
// Сообщения, пришедшие до загрузки модуля, обрабатываются по порядку после неё.

let player = null;

self.addEventListener('message', (event) => {
  const message = event.data;
  if (player === null) {
    player = import(message.coreUrl).then((core) => core.createWorkerPlayer(self));
  }
  player
    .then((handle) => handle(message))
    .catch((error) => self.postMessage({ type: 'log', line: `classic worker failed: ${error}` }));
});
