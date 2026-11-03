//! Случаи сценария `codecs` и их чистый разбор: какой кодек обязана выбрать страница при какой
//! комбинации «настройка экстеншена × настройки плеера» и что она обязана ответить на
//! вопросы про кодеки. Здесь нет браузера — только таблица и судья отчёта (его проверяют тесты).

use crate::environment::PlayerConfigProfile;
use crate::site::{PageReport, PageStatus};

/// Ключ настройки в `storage.local` экстеншена (зеркало `CODEC_SETTING_KEY` в `shared/codec-storage.ts`).
pub(super) const CODEC_SETTING_KEY: &str = "codecSetting";
/// Идентификатор динамического скрипта-курьера (зеркало `CARRIER_SCRIPT_ID` в `shared/codec-policy.ts`).
pub(super) const CARRIER_SCRIPT_ID: &str = "fastiplayer-codec-policy";

/// Какое семейство выбирает страница (порядок вопросов страницы: AV1, VP9, H.264).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChosenCodec {
    /// Политики нет: страница берёт самое «жадное» — AV1.
    Av1,
    /// AV1 отвергнут, VP9 доступен.
    Vp9,
    /// Отвергнуты AV1 и VP9.
    H264,
}

impl ChosenCodec {
    fn family(self) -> &'static str {
        match self {
            Self::Av1 => "av1",
            Self::Vp9 => "vp9",
            Self::H264 => "h264",
        }
    }

    /// Должен ли браузер при этом выборе ОТКАЗЫВАТЬ в AV1.
    fn refuses_av1(self) -> bool {
        self != Self::Av1
    }

    /// Должен ли браузер при этом выборе ОТКАЗЫВАТЬ в VP9.
    fn refuses_vp9(self) -> bool {
        self == Self::H264
    }
}

/// Один случай сценария.
#[derive(Debug, Clone, Copy)]
pub(super) struct CodecCase {
    /// Короткое имя для таблицы результатов.
    pub(super) name: &'static str,
    /// Настройки плеера в окружении случая.
    pub(super) player_config: PlayerConfigProfile,
    /// Значение настройки `codecSetting`, которое робот пишет в `storage.local` до hello
    /// (`None` — не трогать, остаётся `auto`).
    pub(super) setting: Option<&'static str>,
    /// Что страница обязана выбрать.
    pub(super) expected: ChosenCodec,
    /// Файл динамического курьера, который обязан быть зарегистрирован, пока действует политика
    /// (`None` — регистрации быть не должно).
    pub(super) expected_carrier_file: Option<&'static str>,
}

/// Все случаи (решения 43, 64, 189).
pub(super) const CASES: &[CodecCase] = &[
    // Умолчание игрока (VP9 первым): `auto` отказывает AV1 — сайты уходят на VP9 (решение 189).
    CodecCase {
        name: "auto-default-codec-order",
        player_config: PlayerConfigProfile::Clean,
        setting: None,
        expected: ChosenCodec::Vp9,
        expected_carrier_file: Some("codec-policy-av1.js"),
    },
    // H.264 первым в списке игрока при обычном рендере: отказать AV1, VP9 и HEVC (и во фрейме).
    CodecCase {
        name: "auto-h264-first-order",
        player_config: PlayerConfigProfile::H264FirstCodecOrder,
        setting: None,
        expected: ChosenCodec::H264,
        expected_carrier_file: Some("codec-policy-av1-vp9-hevc.js"),
    },
    // GLES2: рендер ограничивает метку до H.264 (решение 43).
    CodecCase {
        name: "auto-gles2-player",
        player_config: PlayerConfigProfile::Gles2Vaapi,
        setting: None,
        expected: ChosenCodec::H264,
        expected_carrier_file: Some("codec-policy-av1-vp9-hevc.js"),
    },
    // «Prefer VP9 (no AV1)» при чистом плеере.
    CodecCase {
        name: "prefer-vp9",
        player_config: PlayerConfigProfile::Clean,
        setting: Some("prefer_vp9"),
        expected: ChosenCodec::Vp9,
        expected_carrier_file: Some("codec-policy-av1.js"),
    },
    // «Always H.264»: отказ AV1, VP9, HEVC и VP8 при любом плеере.
    CodecCase {
        name: "always-h264",
        player_config: PlayerConfigProfile::Clean,
        setting: Some("always_h264"),
        expected: ChosenCodec::H264,
        expected_carrier_file: Some("codec-policy-av1-vp9-hevc-vp8.js"),
    },
    // «Off» перекрывает и метку плеера (GLES2): страница берёт AV1.
    CodecCase {
        name: "off-gles2-player",
        player_config: PlayerConfigProfile::Gles2Vaapi,
        setting: Some("off"),
        expected: ChosenCodec::Av1,
        expected_carrier_file: None,
    },
];

/// Значение ключа из строки `ключ=значение;ключ=значение` (проверка `answers` страницы).
fn answer<'a>(answers: &'a str, key: &str) -> Option<&'a str> {
    answers
        .split(';')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

fn check_detail<'a>(report: &'a PageReport, name: &str) -> Option<&'a str> {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .map(|check| check.detail.as_str())
}

/// Судья: провалы отчёта страницы для случая (пусто — случай пройден).
pub(super) fn judge_report(case: &CodecCase, report: &PageReport) -> Vec<String> {
    let mut failures = Vec::new();
    if report.status != PageStatus::Pass {
        let failed: Vec<String> = report
            .checks
            .iter()
            .filter(|check| !check.ok)
            .map(|check| format!("{} ({})", check.name, check.detail))
            .collect();
        failures.push(format!(
            "the page did not pass ({:?}); failed checks: {}",
            report.status,
            failed.join("; ")
        ));
    }
    let expected_choice = format!("chosen_codec={}", case.expected.family());
    match check_detail(report, "chosen-codec") {
        Some(choice) if choice == expected_choice => {}
        other => failures.push(format!(
            "expected {expected_choice}, the page said {other:?}"
        )),
    }
    if check_detail(report, "playing").is_none() {
        failures.push("the chosen video never reached the 'playing' check".to_owned());
    }
    // Ответы главной страницы и вложенного cross-origin фрейма — одни и те же правила.
    for (label, name) in [
        ("main page", "answers"),
        ("cross-origin frame", "frame-answers"),
    ] {
        match check_detail(report, name) {
            Some(answers) => failures.extend(judge_answers(case.expected, label, answers)),
            None => failures.push(format!("no '{name}' check from the {label}")),
        }
    }
    failures
}

/// Ответы браузера на вопросы про кодеки одной страницы/фрейма.
fn judge_answers(expected: ChosenCodec, label: &str, answers: &str) -> Vec<String> {
    let mut failures = Vec::new();
    let mut expect = |key: &str, wanted: &str| {
        let got = answer(answers, key);
        if got != Some(wanted) {
            failures.push(format!("{label}: {key} expected {wanted}, got {got:?}"));
        }
    };
    // AV1 и VP9: отказ во всех вопросах, если политика их отвергает; иначе ответ браузера.
    if expected.refuses_av1() {
        expect("is_type_supported_av1", "false");
        expect("can_play_type_av1", "empty");
        expect("media_capabilities_av1", "false");
    } else {
        expect("is_type_supported_av1", "true");
        expect("media_capabilities_av1", "true");
    }
    if expected.refuses_vp9() {
        expect("is_type_supported_vp9", "false");
        expect("can_play_type_vp9", "empty");
        expect("media_capabilities_vp9", "false");
    } else {
        expect("is_type_supported_vp9", "true");
        expect("media_capabilities_vp9", "true");
    }
    // H.264 звук/видео не отвергается никогда.
    expect("is_type_supported_h264", "true");
    // HLS: отказ при любой настройке (решение 64).
    expect("hls_can_play_type", "empty");
    expect("hls_is_type_supported", "false");
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{ReportCheck, RunId};

    const H264_ANSWERS: &str = "is_type_supported_av1=false;can_play_type_av1=empty;media_capabilities_av1=false;is_type_supported_vp9=false;can_play_type_vp9=empty;media_capabilities_vp9=false;is_type_supported_h264=true;hls_can_play_type=empty;hls_is_type_supported=false";
    const VP9_ANSWERS: &str = "is_type_supported_av1=false;can_play_type_av1=empty;media_capabilities_av1=false;is_type_supported_vp9=true;can_play_type_vp9=probably;media_capabilities_vp9=true;is_type_supported_h264=true;hls_can_play_type=empty;hls_is_type_supported=false";
    const OPEN_ANSWERS: &str = "is_type_supported_av1=true;can_play_type_av1=probably;media_capabilities_av1=true;is_type_supported_vp9=true;media_capabilities_vp9=true;is_type_supported_h264=true;hls_can_play_type=empty;hls_is_type_supported=false";

    fn check(name: &str, ok: bool, detail: &str) -> ReportCheck {
        ReportCheck {
            name: name.to_owned(),
            ok,
            detail: detail.to_owned(),
        }
    }

    fn report(status: PageStatus, choice: &str, answers: &str, frame_answers: &str) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "codec-choice".to_owned(),
            status,
            checks: vec![
                check("answers", true, answers),
                check("chosen-codec", true, choice),
                check("frame-answers", true, frame_answers),
                check("playing", true, "frames=60"),
            ],
            user_agent: String::new(),
            elapsed_ms: 1,
            metrics: Default::default(),
        }
    }

    fn case(name: &str) -> &'static CodecCase {
        CASES
            .iter()
            .find(|case| case.name == name)
            .expect("known case")
    }

    #[test]
    fn answer_reads_exact_keys_not_prefixes() {
        assert_eq!(answer("a_b=1;a=2", "a"), Some("2"));
        assert_eq!(answer("a_b=1", "a"), None);
        assert_eq!(answer("k=", "k"), Some(""));
    }

    #[test]
    fn a_page_that_follows_the_policy_passes() {
        let h264 = report(
            PageStatus::Pass,
            "chosen_codec=h264",
            H264_ANSWERS,
            H264_ANSWERS,
        );
        assert_eq!(
            judge_report(case("auto-gles2-player"), &h264),
            Vec::<String>::new()
        );
        let vp9 = report(
            PageStatus::Pass,
            "chosen_codec=vp9",
            VP9_ANSWERS,
            VP9_ANSWERS,
        );
        assert_eq!(judge_report(case("prefer-vp9"), &vp9), Vec::<String>::new());
        let open = report(
            PageStatus::Pass,
            "chosen_codec=av1",
            OPEN_ANSWERS,
            OPEN_ANSWERS,
        );
        assert_eq!(
            judge_report(case("off-gles2-player"), &open),
            Vec::<String>::new()
        );
    }

    #[test]
    fn choosing_av1_under_a_policy_fails() {
        let leaked = report(
            PageStatus::Pass,
            "chosen_codec=av1",
            OPEN_ANSWERS,
            OPEN_ANSWERS,
        );
        let failures = judge_report(case("always-h264"), &leaked);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("expected chosen_codec=h264"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("is_type_supported_av1 expected false"))
        );
    }

    #[test]
    fn prefer_vp9_must_not_refuse_vp9_and_must_refuse_av1() {
        // Страница выбрала H.264, потому что VP9 тоже отвергли: это лишний отказ.
        let over_refused = report(
            PageStatus::Pass,
            "chosen_codec=h264",
            H264_ANSWERS,
            H264_ANSWERS,
        );
        let failures = judge_report(case("prefer-vp9"), &over_refused);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("expected chosen_codec=vp9"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("is_type_supported_vp9 expected true"))
        );
    }

    #[test]
    fn a_refusal_without_a_policy_fails() {
        let refused = report(
            PageStatus::Pass,
            "chosen_codec=h264",
            H264_ANSWERS,
            H264_ANSWERS,
        );
        let failures = judge_report(case("off-gles2-player"), &refused);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("expected chosen_codec=av1"))
        );
    }

    #[test]
    fn the_frame_must_follow_the_policy_too() {
        let main_only = report(
            PageStatus::Pass,
            "chosen_codec=h264",
            H264_ANSWERS,
            OPEN_ANSWERS,
        );
        let failures = judge_report(case("auto-gles2-player"), &main_only);
        assert!(
            failures
                .iter()
                .any(|failure| failure.starts_with("cross-origin frame:"))
        );
        assert!(
            !failures
                .iter()
                .any(|failure| failure.starts_with("main page:"))
        );
    }

    #[test]
    fn hls_must_be_refused_even_without_a_policy() {
        let hls_open = OPEN_ANSWERS.replace("hls_can_play_type=empty", "hls_can_play_type=maybe");
        let report = report(
            PageStatus::Pass,
            "chosen_codec=av1",
            &hls_open,
            OPEN_ANSWERS,
        );
        let failures = judge_report(case("off-gles2-player"), &report);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("hls_can_play_type expected empty"))
        );
    }

    #[test]
    fn a_failed_page_and_missing_checks_are_reported() {
        let mut broken = report(
            PageStatus::Fail,
            "chosen_codec=av1",
            OPEN_ANSWERS,
            OPEN_ANSWERS,
        );
        broken
            .checks
            .retain(|check| check.name != "playing" && check.name != "frame-answers");
        let failures = judge_report(case("off-gles2-player"), &broken);
        assert!(
            failures
                .iter()
                .any(|failure| failure.starts_with("the page did not pass"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("never reached the 'playing'"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("no 'frame-answers' check"))
        );
    }

    #[test]
    fn carrier_files_follow_the_refusal_sets() {
        assert_eq!(case("off-gles2-player").expected_carrier_file, None);
        assert_eq!(
            case("prefer-vp9").expected_carrier_file,
            Some("codec-policy-av1.js")
        );
        assert_eq!(
            case("auto-gles2-player").expected_carrier_file,
            Some("codec-policy-av1-vp9-hevc.js")
        );
        assert_eq!(
            case("always-h264").expected_carrier_file,
            Some("codec-policy-av1-vp9-hevc-vp8.js")
        );
    }
}
