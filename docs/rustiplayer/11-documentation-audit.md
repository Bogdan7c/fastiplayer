# 11. Аудит документации

Дата аудита: 2026-05-15.

## Что было устаревшим

- Фазовые документы Phase 8.5, Phase 9, Phase 10 больше описывали процесс, а не
  текущее состояние.
- Live seek/timeline и smooth playback session-документы были длинными prompt
  plans, частично уже закрытыми кодом.
- `fix.md` был рабочим заданием на исправления, а не архитектурной документацией.
- README ссылался на исторические планы как на рабочую навигацию.

Решение: удалить устаревшие фазовые/session файлы и заменить их компактными
документами о текущем состоянии.

## Исправленные неточности

- YouTube startup больше не нужно описывать как блокирующий UI startup: shell
  запускает resolver на background thread. При этом сам `service-youtube` API
  остаётся blocking-вызовом service layer.
- Runtime durable index/database больше не является планом текущей архитектуры.
- Production HDR больше не "future after Phase 10": локальный VP9/P010 HDR-to-SDR
  path существует, но gated capabilities.
- Unknown video codec не должен описываться как VP9 fallback: demuxer использует
  `unknown_video`.
- Worker render bridge больше не создаёт WGPU views внутри player thread:
  texture views создаются на render thread через lease provider.

## Логические ошибки, которые убраны из доков

- Смешение "capability probe" и "decoder validation" заменено правилом:
  recoverable probe не является fatal reject.
- "zero-copy" описан как memory contract, а не как настройка config.
- HDR side metadata не считается достаточным HDR-признаком без PQ/HLG transfer.
- P010 storage layout отделён от renderer plane kind.
- `app-egui` описан как shell, а не как владелец playback pipeline.

## Что осталось сознательно зафиксированным как долг

Подробно см. [10. Module Boundaries and Debt](10-module-boundaries-and-debt.md).

Коротко:

- `PlayerSession` всё ещё крупный orchestration object.
- `PlaybackPipeline` остаётся широким `pub(crate)` хранилищем.
- `AppState::player_snapshot()` имеет side effect публикации desktop snapshot.
- YouTube service API ещё не capability-aware.
- `webm-demux` грубо классифицирует non-audio tracks как video.

## Self-review checklist

- Нет ссылок на удалённые phase/session документы.
- Текущая production matrix соответствует `codec-core`, `capability-core`,
  `render-core`, `render-wgpu-video`, `render-wgpu-shell` и `video-vaapi`.
- Config schema version указан как `2`.
- Zero-copy описан как обязательный invariant.
- Места с неидеальными границами вынесены в отдельный документ.
