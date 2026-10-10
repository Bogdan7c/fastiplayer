# Resume у конца (B2, ux-edge-cases-2)

- Политика: `state/strong_media_open/pending/resume_policy.rs`, `should_restart_from_beginning(saved, duration)`.
- Правило: остаток < 5 % длительности ИЛИ < 10 с (строго; ровно на границе восстанавливаем). Неизвестная длительность / live — правило не применяется.
- Единственная точка применения: `plan_post_installed_startup_position` (resume.rs) — длительность известна только после Installed. Checkpoint и его формат не менялись.
- Для native HLS (`AdoptPreparedInitialPosition`) open уже позиционирован у конца, поэтому план возвращает `Restore { SeekTo(0) }`, а не `Continue`.
- Пороги захардкожены константами (в config есть только bool `player.resume_last_position`).
