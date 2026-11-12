//! Приговор строк `instant-next` и `reset-next` (сессия 16a3, находка 3 живой проверки 16a2):
//! доигравшая серия 1 не перематывается и не играет снова, пока плеер ставит серию 2.
//!
//! Живой дефект: после конца серии 1 плеер играл её начало (0,4 с на Tortuga, до 5 с на klonua),
//! потом показывал серию 2. Корень (решение 313): сайт в такте `ended` сбрасывает доигравший
//! элемент на ноль, а плеер исполнял эту перемотку со старой волей «играть». Строка `reset-next`
//! воспроизводит этот порядок; `instant-next` — порядок PlayerJS (`play()` до `sourceopen`).
//!
//! Признаки в журнале плеера между концом показанного и загрузкой следующего поколения — в этом
//! промежутке плееру нечего ни перематывать, ни играть:
//! - перемотка ядра в любой стадии: первый кадр после перемотки, перемотка, ждущая данных (у
//!   плеера может уже не быть начала серии — тогда она застревает, и звука нет, но плеер всё равно
//!   пытался вернуть прошлую серию), завершённая перемотка («Final seek commit completed»);
//! - старт звука («Audio stream started») — на машине с настоящим звуком.

/// Как страница меняет серию сразу после конца (обе формы — живые журналы 16a2-16a3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EpisodeSwapOrder {
    /// Tortuga (PlayerJS): новый источник тем же кодом, `play()` и перемотка на 0 до `sourceopen`.
    PlayBeforeOpen,
    /// klonua (Ashdi): сброс доигравшего элемента на 0 в такте `ended`, потом снятие источника и
    /// серия 2 через ~1 с.
    ResetBeforeSwap,
}

/// Строки журнала плеера, по порядку которых судит этот приговор.
pub(super) const EPISODE_CHANGE_MARKERS: [&str; 6] = [
    PLAYBACK_ENDED_MARKER,
    SEEK_FIRST_FRAME_MARKER,
    SEEK_WAITING_MARKER,
    SEEK_ACCEPTED_MARKER,
    AUDIO_STARTED_MARKER,
    GENERATION_LOADED_MARKER,
];

/// Плеер доиграл показанное поколение.
const PLAYBACK_ENDED_MARKER: &str = "Browser playback ended";
/// Ядро довело перемотку показанного медиа до показа (и перемотка, и перезапуск с начала).
const SEEK_ACCEPTED_MARKER: &str = "Final seek commit completed";
/// Ядро декодировало первый кадр после перемотки.
const SEEK_FIRST_FRAME_MARKER: &str = "First post-seek decoded frame observed";
/// Перемотка ядра ждёт данных дольше 0,5 с (данных начала серии у плеера уже нет).
const SEEK_WAITING_MARKER: &str = "Active seek transaction is still waiting";
/// Звук пошёл в колонки.
const AUDIO_STARTED_MARKER: &str = "Audio stream started";
/// Плеер загрузил поколение страницы (номер — в поле `generation=`).
const GENERATION_LOADED_MARKER: &str = "Browser stream generation loaded";

/// Провалы: после конца серии 1 и до загрузки следующей плеер перематывал или запускал звук, или улик нет.
pub(super) fn previous_episode_replay_failures(episode_change: &[String]) -> Vec<String> {
    let Some(end_index) = episode_change
        .iter()
        .position(|line| line.contains(PLAYBACK_ENDED_MARKER))
    else {
        return vec!["the player never reported the end of episode 1".to_owned()];
    };
    let after_end = &episode_change[end_index + 1..];
    let Some(next_loaded) = after_end
        .iter()
        .position(|line| line.contains(GENERATION_LOADED_MARKER))
    else {
        return vec![
            "the player never loaded the next generation after episode 1 ended".to_owned(),
        ];
    };
    let restarts: Vec<&String> = after_end[..next_loaded]
        .iter()
        .filter(|line| moves_or_plays_the_shown_media(line))
        .collect();
    if restarts.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "episode 1 was moved or played again between its end and the next generation ({} seek/audio line(s)): {restarts:?}",
        restarts.len()
    )]
}

/// Строка — перемотка ядра в любой стадии или старт звука.
fn moves_or_plays_the_shown_media(line: &str) -> bool {
    [
        SEEK_FIRST_FRAME_MARKER,
        SEEK_WAITING_MARKER,
        SEEK_ACCEPTED_MARKER,
        AUDIO_STARTED_MARKER,
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn audio_only_after_the_next_generation_loaded_is_clean() {
        let journal = lines(&[
            "INFO Audio stream started",
            "INFO Browser stream generation loaded generation=1",
            "INFO Browser playback ended presented_frames=700",
            "INFO Browser stream generation loaded generation=2",
            "INFO Audio stream started",
        ]);
        assert_eq!(
            previous_episode_replay_failures(&journal),
            Vec::<String>::new()
        );
    }

    #[test]
    fn audio_between_the_end_and_the_next_generation_is_the_replay_defect() {
        // Форма живого журнала 16a2 (Tortuga): конец, звук серии 1 с нуля, потом серия 2.
        let journal = lines(&[
            "INFO Browser playback ended presented_frames=12228",
            "INFO Audio stream started",
            "INFO Browser stream generation loaded generation=2",
        ]);
        let failures = previous_episode_replay_failures(&journal);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("played again"), "{failures:?}");
    }

    #[test]
    fn a_seek_of_the_ended_episode_is_the_defect_even_without_real_audio() {
        let journal = lines(&[
            "INFO Browser playback ended presented_frames=411 audio_output=simulated",
            "INFO Final seek commit completed kind=\"seek_acceptance\" target_ms=0 committed_ms=0",
            "INFO Browser stream generation loaded generation=2",
            "INFO Final seek commit completed kind=\"seek_acceptance\" target_ms=80 committed_ms=80",
        ]);
        let failures = previous_episode_replay_failures(&journal);
        assert_eq!(
            failures.len(),
            1,
            "only the seek before the next generation counts: {failures:?}"
        );
    }

    #[test]
    fn missing_end_or_missing_next_generation_is_not_a_pass() {
        assert_eq!(
            previous_episode_replay_failures(&lines(&["INFO Audio stream started"])),
            vec!["the player never reported the end of episode 1".to_owned()]
        );
        assert_eq!(
            previous_episode_replay_failures(&lines(&["INFO Browser playback ended"])),
            vec!["the player never loaded the next generation after episode 1 ended".to_owned()]
        );
    }

    #[test]
    fn a_seek_of_the_ended_episode_stuck_without_data_is_the_defect_too() {
        // Форма стенда `reset-next` без починки: данных начала серии у плеера нет, перемотка застряла.
        let journal = lines(&[
            "INFO Browser playback ended presented_frames=412 audio_output=simulated",
            "WARN Active seek transaction is still waiting kind=\"seek\" blocker=demux age_ms=500",
            "WARN Active seek transaction is still waiting kind=\"seek\" blocker=demux age_ms=1000",
            "INFO Browser stream generation loaded generation=2",
        ]);
        let failures = previous_episode_replay_failures(&journal);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("2 seek/audio line(s)"), "{failures:?}");
    }
}
