//! Тесты разбора командной строки (вынесены из `cli.rs` по пределу размера модуля).

use super::*;

fn parse_words(words: &[&str]) -> Result<Parsed, UsageError> {
    parse(words.iter().map(|word| (*word).to_owned()))
}

fn invocation(words: &[&str]) -> Invocation {
    match parse_words(words).expect("parses") {
        Parsed::Run(invocation) => *invocation,
        Parsed::Help => panic!("expected a run"),
    }
}

#[test]
fn defaults_are_both_browsers_virtual_display_clean_config() {
    let run = invocation(&["smoke"]);
    assert_eq!(run.command, Command::Smoke);
    assert_eq!(
        run.browsers,
        vec![BrowserKind::Chromium, BrowserKind::Firefox]
    );
    assert_eq!(run.settings.display, DisplayTarget::Virtual);
    assert_eq!(run.settings.player_config, PlayerConfigProfile::Clean);
    assert!(
        run.settings
            .work_root
            .ends_with("target/browser-bridge-bench/work")
    );
    assert!(run.settings.player_bin_dir.ends_with("target/debug"));
}

#[test]
fn flags_may_surround_the_command_and_repeat() {
    let run = invocation(&[
        "--browser",
        "firefox",
        "--display",
        "real",
        "--player-config",
        "gles2-vaapi",
        "--extra-extension",
        "/a",
        "all",
        "--extra-extension",
        "/b",
        "--keep-work",
    ]);
    assert_eq!(run.command, Command::All);
    assert_eq!(run.browsers, vec![BrowserKind::Firefox]);
    assert_eq!(run.settings.display, DisplayTarget::Real);
    assert_eq!(run.settings.player_config, PlayerConfigProfile::Gles2Vaapi);
    assert_eq!(
        run.settings.extra_extensions,
        vec![PathBuf::from("/a"), PathBuf::from("/b")]
    );
    assert!(run.settings.keep_work);
}

#[test]
fn new_capture_commands_are_recognised() {
    assert_eq!(invocation(&["capture"]).command, Command::Capture);
    assert_eq!(
        invocation(&["player-commands"]).command,
        Command::PlayerCommands
    );
}

#[test]
fn page_flag_repeats_and_builds_a_filter() {
    let run = invocation(&["capture", "--page", "page-freeze", "--page", "mse-fmp4"]);
    // page-freeze не страница capture, но mse-fmp4 — да: выбор пересекается.
    assert_eq!(run.pages.ids(), ["page-freeze", "mse-fmp4"]);
    assert!(run.pages.admits("mse-fmp4") && !run.pages.admits("mse-webm"));
    assert!(invocation(&["capture"]).pages.selects_everything());
    let ends = invocation(&[
        "page-ends",
        "--page",
        "source-withdrawn",
        "--page",
        "source-withdrawn",
    ]);
    assert_eq!(ends.command, Command::PageEnds);
    assert_eq!(ends.pages.ids(), ["source-withdrawn"]);
}

#[test]
fn page_flag_rejects_unknown_pages_foreign_pages_and_commands_without_pages() {
    let cases: [(&[&str], &str); 6] = [
        (
            &["capture", "--page", "no-such-page"],
            "unknown page 'no-such-page'",
        ),
        (
            &["player-commands", "--page", "page-freeze"],
            "is a page of the PlayerCommands",
        ),
        (
            &["capture", "--page", "page-freeze"],
            "none of [\"page-freeze\"] is a page of the Capture",
        ),
        (
            &["page-ends", "--page", "mse-fmp4"],
            "is a page of the PageEnds",
        ),
        (
            &["smoke", "--page", "mse-fmp4"],
            "--page does not apply to the Smoke",
        ),
        (&["capture", "--page"], "--page needs a value"),
    ];
    for (words, expected) in cases {
        let error = parse_words(words).expect_err("must be rejected");
        assert!(error.to_string().contains(expected), "{words:?}: {error}");
    }
}

#[test]
fn player_commands_take_a_page_filter_for_the_live_stream_pages() {
    let run = invocation(&["player-commands", "--page", "live-unload-worker"]);
    assert_eq!(run.command, Command::PlayerCommands);
    assert!(run.pages.admits("live-unload-worker") && !run.pages.admits("mse-fmp4"));
    assert!(invocation(&["player-commands"]).pages.selects_everything());
}

#[test]
fn page_flag_works_for_pages_and_all_with_any_known_page() {
    assert_eq!(invocation(&["pages", "--page", "pip"]).pages.ids(), ["pip"]);
    let all = invocation(&["all", "--page", "page-freeze"]);
    assert_eq!(all.command, Command::All);
    assert_eq!(all.pages.ids(), ["page-freeze"]);
}

#[test]
fn the_09c_commands_are_recognised_and_only_attacks_take_pages() {
    assert_eq!(invocation(&["attacks"]).command, Command::Attacks);
    assert_eq!(
        invocation(&["local-address"]).command,
        Command::LocalAddress
    );
    assert_eq!(
        invocation(&["blocker-matrix"]).command,
        Command::BlockerMatrix
    );
    let one = invocation(&["attacks", "--page", "attack-garbage"]);
    assert_eq!(one.pages.ids(), ["attack-garbage"]);
    for words in [
        &["local-address", "--page", "video-src-local"][..],
        &["blocker-matrix", "--page", "mse-fmp4"][..],
        &["attacks", "--page", "mse-fmp4"][..],
    ] {
        assert!(parse_words(words).is_err(), "{words:?} must be rejected");
    }
}

#[test]
fn page_subtitles_takes_its_own_pages_in_the_page_filter() {
    let run = invocation(&["page-subtitles", "--page", "subtitles-xhr"]);
    assert_eq!(run.command, Command::PageSubtitles);
    assert_eq!(run.pages.ids(), ["subtitles-xhr"]);
    let error = parse_words(&["page-subtitles", "--page", "mse-fmp4"]).expect_err("foreign page");
    assert!(
        error.to_string().contains("is a page of the PageSubtitles"),
        "{error}"
    );
}

#[test]
fn hls_live_is_a_command_of_its_own_without_a_page_filter() {
    assert_eq!(invocation(&["hls-live"]).command, Command::HlsLive);
    assert!(parse_words(&["hls-live", "--page", "hls-live"]).is_err());
}

#[test]
fn live_ends_is_a_command_of_its_own_listed_in_the_help() {
    assert_eq!(invocation(&["live-ends"]).command, Command::LiveEnds);
    assert!(parse_words(&["live-ends", "--page", "hls-live-ends"]).is_err());
    assert!(USAGE.contains("  live-ends "));
}

#[test]
fn page_reload_is_a_command_with_its_own_page() {
    assert_eq!(invocation(&["page-reload"]).command, Command::PageReload);
    assert!(
        invocation(&["page-reload", "--page", "page-reload"])
            .pages
            .admits("page-reload")
    );
    assert!(parse_words(&["page-reload", "--page", "mse-webm"]).is_err());
}

#[test]
fn tab_return_is_a_command_with_its_own_page() {
    assert_eq!(invocation(&["tab-return"]).command, Command::TabReturn);
    assert!(
        invocation(&["tab-return", "--page", "tab-return"])
            .pages
            .admits("tab-return")
    );
    assert!(parse_words(&["tab-return", "--page", "mse-webm"]).is_err());
}

#[test]
fn page_return_is_a_command_with_its_own_pages() {
    assert_eq!(invocation(&["page-return"]).command, Command::PageReturn);
    let one = invocation(&["page-return", "--page", "return-offset"]);
    assert!(one.pages.admits("return-offset"));
    assert!(!one.pages.admits("return-kit"));
    // Страница стенда, которой у команды нет, — ошибка, а не пустой «успешный» прогон.
    assert!(parse_words(&["page-return", "--page", "mse-webm"]).is_err());
}

#[test]
fn auto_handoff_is_a_command_with_its_own_pages() {
    assert_eq!(invocation(&["auto-handoff"]).command, Command::AutoHandoff);
    let one = invocation(&["auto-handoff", "--page", "auto-film"]);
    assert!(one.pages.admits("auto-film"));
    assert!(!one.pages.admits("auto-clip"));
    // Страница, которой у команды нет, — ошибка, а не пустой «успешный» прогон.
    assert!(parse_words(&["auto-handoff", "--page", "mse-webm"]).is_err());
}

#[test]
fn page_ends_command_is_recognised() {
    assert_eq!(invocation(&["page-ends"]).command, Command::PageEnds);
}

#[test]
fn replay_needs_a_recording_and_reads_min_frames() {
    let run = invocation(&[
        "replay",
        "--recording",
        "/tmp/a.fprec",
        "--browser",
        "firefox",
        "--min-frames",
        "120",
    ]);
    assert_eq!(run.command, Command::Replay);
    let replay = run.replay.expect("replay options");
    assert_eq!(replay.recording, PathBuf::from("/tmp/a.fprec"));
    assert_eq!(replay.min_frames, 120);
    assert_eq!(
        invocation(&["replay", "--recording", "/tmp/a.fprec"])
            .replay
            .map(|o| o.min_frames),
        Some(DEFAULT_MIN_FRAMES)
    );
    let cases: [(&[&str], &str); 4] = [
        (&["replay"], "replay needs --recording"),
        (
            &["smoke", "--recording", "/x"],
            "--recording applies only to the replay",
        ),
        (
            &["replay", "--recording", "/x", "--min-frames", "0"],
            "positive integer",
        ),
        (
            &["replay", "--recording", "/x", "--page", "mse-fmp4"],
            "--page does not apply",
        ),
    ];
    for (words, expected) in cases {
        let error = parse_words(words).expect_err("must be rejected");
        assert!(error.to_string().contains(expected), "{words:?}: {error}");
    }
}

#[test]
fn generate_recording_needs_an_output_and_defaults_the_asset() {
    let run = invocation(&["generate-recording", "--output", "/tmp/s.fprec"]);
    assert_eq!(
        run.generation,
        Some(GenerationOptions {
            output: PathBuf::from("/tmp/s.fprec"),
            asset: "h264-aac".into()
        })
    );
    assert!(parse_words(&["generate-recording"]).is_err());
    assert!(parse_words(&["smoke", "--output", "/x"]).is_err());
}

#[test]
fn help_wins_over_everything() {
    assert!(matches!(parse_words(&["--help"]), Ok(Parsed::Help)));
    assert!(matches!(parse_words(&["smoke", "-h"]), Ok(Parsed::Help)));
}

#[test]
fn usage_errors_are_reported_with_the_offending_value() {
    let cases: [(&[&str], &str); 6] = [
        (&[], "a command is required"),
        (&["smoke", "pages"], "only one command"),
        (&["bogus"], "unknown command 'bogus'"),
        (&["--nope", "smoke"], "unknown option --nope"),
        (&["--browser"], "--browser needs a value"),
        (&["--browser", "safari", "smoke"], "got 'safari'"),
    ];
    for (words, expected) in cases {
        let error = parse_words(words).expect_err("must be rejected");
        assert!(error.to_string().contains(expected), "{words:?}: {error}");
    }
}

#[test]
fn every_command_name_parses_back_to_the_same_command() {
    for command in command_names::ALL_COMMANDS {
        assert_eq!(parse_command(command.cli_name()), Ok(command));
    }
}

#[test]
fn a_child_gets_exactly_the_parent_settings_back() {
    let parent = invocation(&[
        "all",
        "--display",
        "virtual",
        "--player-config",
        "gles2-vaapi",
        "--extra-extension",
        "/ext/blocker",
        "--keep-work",
        "--work-root",
        "/w",
        "--media-cache",
        "/m",
        "--player-bin-dir",
        "/target/release",
        "--browser-path-firefox",
        "/opt/firefox",
    ]);
    let request = ChildRequest {
        command: Command::Episodes,
        browser: BrowserKind::Firefox,
        page: Some("episode-gap"),
        results_file: Path::new("/w/jobs/007.json"),
    };
    let arguments = child_arguments(&request, &parent.settings).expect("arguments");
    let child = invocation(&arguments.iter().map(String::as_str).collect::<Vec<_>>());

    assert_eq!(child.command, Command::Episodes);
    assert_eq!(child.browsers, [BrowserKind::Firefox]);
    assert_eq!(child.pages.ids(), ["episode-gap"]);
    assert_eq!(child.jobs, JobCount::ONE, "a child never splits again");
    assert_eq!(child.results_file, Some(PathBuf::from("/w/jobs/007.json")));
    let (expected, actual) = (&parent.settings, &child.settings);
    assert_eq!(actual.display, expected.display);
    assert_eq!(actual.player_config, expected.player_config);
    assert_eq!(actual.extra_extensions, expected.extra_extensions);
    assert_eq!(actual.keep_work, expected.keep_work);
    assert_eq!(actual.work_root, expected.work_root);
    assert_eq!(actual.media_cache, expected.media_cache);
    assert_eq!(actual.player_bin_dir, expected.player_bin_dir);
    assert_eq!(
        actual.browser_executables.firefox,
        expected.browser_executables.firefox
    );
    assert_eq!(actual.browser_executables.chromium, None);
}

#[test]
fn a_scenario_only_player_profile_is_not_passed_silently() {
    let mut settings = invocation(&["capture"]).settings;
    settings.player_config = PlayerConfigProfile::SmallFileWindow;
    let request = ChildRequest {
        command: Command::Capture,
        browser: BrowserKind::Chromium,
        page: None,
        results_file: Path::new("/r.json"),
    };
    assert!(child_arguments(&request, &settings).is_err());
}

#[test]
fn jobs_default_to_the_machine_and_stay_single_on_the_real_screen() {
    assert_eq!(invocation(&["quick"]).jobs, JobCount::default_for_machine());
    assert_eq!(invocation(&["capture", "--jobs", "3"]).jobs.get(), 3);
    assert_eq!(
        invocation(&["capture", "--display", "real"]).jobs,
        JobCount::ONE
    );
    assert!(parse_words(&["capture", "--display", "real", "--jobs", "2"]).is_err());
    assert!(parse_words(&["replay", "--recording", "/r.fprec", "--jobs", "2"]).is_err());
}

#[test]
fn quick_takes_no_page_filter() {
    assert!(parse_words(&["quick", "--page", "mse-fmp4"]).is_err());
}
