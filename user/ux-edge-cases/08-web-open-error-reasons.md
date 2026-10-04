# Сессия 08. Понятные причины отказа открытия web-media

## Абзац для отправки вместе с этим файлом

> Выполни сессию 08 из приложенного Markdown для Fastiplayer (план `user/ux-edge-cases/`). Сначала прочитай `user/ux-edge-cases/README.md`, `results/03.md` и `results/04.md`. Разрешён только объём, указанный в файле. Соблюдай AGENTS.md. Это сквозное изменение через media-services → media-source-open → app: сначала исследование (какие типизированные причины уже есть и где они теряются) и таблица «причина → текст», затем остановка и согласование со мной простым языком; реализация — после моего решения. Результат запиши в `user/ux-edge-cases/results/08.md`. Работай в main без веток; коммит — только после зелёных проверок и моей ручной приёмки, push не делай.

## Границы

**Зависит от:** 03 (форма типизированной причины на terminal-исходе), 04 (уведомления). Если их `results` нет или статус не PASS — сессия заблокирована.

**Memories:** `mem:media-services/core`, `mem:media-source-open/core`, `mem:media-services/secret-safe-locators-s10b`, `mem:media-services/ytdlp-process-owner-2026-08-05`, `mem:media-services/ytdlp-system-compatibility-aud006-2026-08-23`, `mem:media-services/http-retry-after-aud017-2026-08-24`, `mem:app-egui/queue-owned-web-open-s23-2026-07-22`, `mem:app-egui/native-web-ingress-n05b-2026-08-31`.

**Разрешено:** `MediaPreparationFailureKind` и `PreparationFailed` (`app-egui/src/media_open/types.rs:~473-526`), `media_open/preparation.rs:~62-70`, маппинг ошибок `ProviderOpenError` (`web-media-http/src/lib.rs:~465-506`) и `YtDlpServiceError` (`service-ytdlp/src/error.rs`) в пользовательскую причину, тексты в `ui/playlist/status/presentation.rs:~388`, `url_service_adapter.rs:~521-538`; тесты.

**Запрещено:** менять retry/backoff/timeout политики, VOD endpoint recovery, выбор кандидатов/качества, добавлять cookies/логин для yt-dlp (бэклог), прогресс и отмену добавления URL (сессия 09), раскрывать URL/query/userinfo в тексте.

## Предыстория (факты аудита, сверить с кодом)

1. Все отказы web-open сводятся к `MediaPreparationFailureKind::{DirectOpen, ExtractorOpen, NativeHlsOpen, …}`; поле `kind` в `PreparationFailed` существует **только под `#[cfg(test)]`** (`types.rs:524-526`). Production UI не различает причины.
2. `preparation.rs:~62-70` пишет ошибку в `tracing::warn!` и возвращает только `DirectOpen`.
3. Богатые типы уже есть: `ProviderOpenError::Authentication(CredentialsMissing/Rejected)`, `AccessDenied`, `Timeout`; `YtDlpServiceError::{Timeout, ProcessFailure, ExtractorRejection}`.
4. Пользователь видит «Не удалось открыть выбранный элемент. Предыдущее воспроизведение продолжает работать…» (`presentation.rs:388`) — без причины и подсказки.
5. HTTP 404/410/429 мапятся в `TransportFailure::NetworkUnavailable` (`web-media-http/src/lib.rs:~479-506`) — «нет сети» для мёртвой ссылки. TLS-ошибки — `HttpRequest`, без отдельного смысла.
6. Тексты-смесь: «NetworkError: URL scheme не поддерживается media services», «provider для `ftp` ещё не реализован».
7. Секреты сейчас защищены хорошо (`SecretHttpUrl`, `YtDlpMediaLocator` redaction, `without_url()` у reqwest, stderr yt-dlp скрыт) — **не сломать**.

## Задание

### Часть A. Исследование
1. Дерево причин от источника до UI для: yt-dlp не найден / отключён в настройках / таймаут / extractor отказал (приватное, требуется вход, недоступно в регионе — что из этого вообще различимо по выводу yt-dlp без чтения stderr в UI?), HTTP 401/403/404/410/429/5xx, DNS/connect/TLS/timeout, неподдерживаемая схема, формат не распознан.
2. Где именно теряется тип (каждая точка `map_err`/`warn!` + generic).
3. Таблица «причина → текст + подсказка», например: «yt-dlp не найден — установите yt-dlp (paru -S yt-dlp)», «Видео требует входа в аккаунт», «Сервер ответил: страница не найдена (404)», «Нет соединения с сервером», «Сервер слишком долго не отвечает», «Ссылка устарела».

### Стоп: показать таблицу владельцу
Плюс вопрос: исправлять ли классификацию 404/410/429 в `web-media-http` (рекомендация — да, в этой сессии, это часть корня проблемы).

### Часть B. Реализация
- Одна типизированная пользовательская причина web-open в production (не `cfg(test)`), из media-services/media-source-open в app; форматирование отдельно от классификации.
- Совместимость с причиной из сессии 03 (одна общая модель «почему не открылось» для local/web, если это не связывает слои лишним знанием — обосновать).
- Тексты без URL; хост показывать можно только через существующий safe label.

## Тесты
- fake provider/yt-dlp возвращает каждую причину → ожидаемый текст в status bar / уведомлении;
- реальный локальный HTTP-сервер (как в существующих тестах media-services) отдаёт 404, 403, 429 с Retry-After, обрывает соединение → разные тексты; 404 больше не «нет сети»;
- yt-dlp отсутствует в PATH (подмена PATH в тесте) → «yt-dlp не найден»;
- URL с userinfo/query → в тексте нет секрета (регрессия);
- предыдущее воспроизведение продолжается.

## Ручная приёмка
Ссылка на несуществующую страницу, отключённый Wi-Fi, временно переименованный yt-dlp, приватное видео — каждое сообщение понятно и подсказывает действие.

## Выходной барьер
`results/08.md` по общему шаблону + итоговая таблица текстов. Обновить `mem:media-services/core`, `mem:media-source-open/core` (production-причина отказа), `mem:app-egui/queue-owned-web-open-s23-2026-07-22`.
