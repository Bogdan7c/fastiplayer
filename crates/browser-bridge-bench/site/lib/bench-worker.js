// Модульный Dedicated Worker стенда: страница создаёт его из blob-модуля `import '<адрес этого
// файла>'` и сразу отпускает blob-адрес (как Vite, bench-player.js). Относительный импорт ниже
// разрешается от адреса ЭТОГО модуля (http), а не от blob. Плеер — в bench-worker-core.js.

import { createWorkerPlayer } from './bench-worker-core.js';

const handle = createWorkerPlayer(self);
self.addEventListener('message', (event) => handle(event.data));
