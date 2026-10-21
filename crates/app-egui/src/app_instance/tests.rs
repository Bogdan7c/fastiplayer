use std::cell::RefCell;
use std::ffi::OsString;
use std::rc::Rc;

use fastiplayer_config::ConfigPaths;

use desktop_integration::{FullscreenIntent, InstanceForwardingError};

use super::{
    AppInstanceLease, AppInstanceLeaseError, AppInstanceLeaseIoOperation, AppInstanceLeasePlatform,
    BootstrapOutcome, ForwardedPayload, ProcessArgs, ProcessArgsError, ProcessBootstrapError,
    RunningInstanceForwardingError, RunningInstanceForwardingFailure, UnsafeAppInstanceArtifact,
    bootstrap_with,
};
use crate::fatal_startup::test_support::RecordingPresenter;
use crate::fatal_startup::{FatalStartupError, ProcessConclusion, conclude_process};

#[derive(Debug)]
struct FakeGuard;

struct FakePlatform {
    calls: Rc<RefCell<Vec<&'static str>>>,
    outcome: Result<(), AppInstanceLeaseError>,
}

impl AppInstanceLeasePlatform for FakePlatform {
    fn acquire(&self, _paths: &ConfigPaths) -> Result<AppInstanceLease, AppInstanceLeaseError> {
        self.calls.borrow_mut().push("acquire-lease");
        self.outcome?;
        Ok(AppInstanceLease::from_guard(FakeGuard))
    }
}

fn os_strings(arguments: &[&str]) -> Vec<OsString> {
    arguments.iter().map(OsString::from).collect()
}

#[test]
fn process_args_accepts_zero_one_and_several_media_in_given_order() {
    let mut empty = ProcessArgs::parse(Vec::<OsString>::new()).expect("empty args");
    assert!(empty.initial_media_arguments().is_empty());
    assert!(empty.take_initial_media_arguments().is_empty());

    let one = ProcessArgs::parse(os_strings(&["movie.mkv"])).expect("one media");
    assert_eq!(one.initial_media_arguments(), os_strings(&["movie.mkv"]));

    // Порядок сохраняется ровно таким, каким его передал файловый менеджер.
    let mut several =
        ProcessArgs::parse(os_strings(&["c.mkv", "a.mkv", "b.mkv"])).expect("several media");
    assert_eq!(
        several.initial_media_arguments(),
        os_strings(&["c.mkv", "a.mkv", "b.mkv"])
    );
    assert_eq!(
        several.take_initial_media_arguments(),
        os_strings(&["c.mkv", "a.mkv", "b.mkv"])
    );
    // Аргументы переданы дальше один раз: повторно забрать нечего.
    assert!(several.initial_media_arguments().is_empty());
}

#[test]
fn process_args_rejects_unknown_option_even_among_several_media() {
    assert_eq!(
        ProcessArgs::parse(os_strings(&["--unknown"])),
        Err(ProcessArgsError::UnknownOption)
    );
    assert_eq!(
        ProcessArgs::parse(os_strings(&["one.mkv", "-x", "two.mkv"])),
        Err(ProcessArgsError::UnknownOption)
    );
}

#[test]
fn process_args_double_dash_allows_several_leading_dash_local_paths() {
    let parsed = ProcessArgs::parse(os_strings(&["a.mkv", "--", "-movie.mkv", "--", "-b"]))
        .expect("paths after delimiter");
    // Второй `--` после разделителя — уже обычное имя файла.
    assert_eq!(
        parsed.initial_media_arguments(),
        os_strings(&["a.mkv", "-movie.mkv", "--", "-b"])
    );
}

/// `--fullscreen` до `--` — опция в любом месте (повтор не ошибка), файлы остаются в порядке.
#[test]
fn fullscreen_option_is_recognized_anywhere_before_the_delimiter() {
    let plain = ProcessArgs::parse(os_strings(&["a.mkv"])).expect("plain launch");
    assert_eq!(plain.fullscreen_intent(), FullscreenIntent::LeaveAsIs);

    let leading =
        ProcessArgs::parse(os_strings(&["--fullscreen", "a.mkv", "b.mkv"])).expect("leading flag");
    assert_eq!(leading.fullscreen_intent(), FullscreenIntent::Enter);
    assert_eq!(
        leading.initial_media_arguments(),
        os_strings(&["a.mkv", "b.mkv"])
    );

    let mixed = ProcessArgs::parse(os_strings(&[
        "a.mkv",
        "--fullscreen",
        "b.mkv",
        "--fullscreen",
    ]))
    .expect("flag between and after files, repeated");
    assert_eq!(mixed.fullscreen_intent(), FullscreenIntent::Enter);
    assert_eq!(
        mixed.initial_media_arguments(),
        os_strings(&["a.mkv", "b.mkv"])
    );

    let flag_only = ProcessArgs::parse(os_strings(&["--fullscreen"])).expect("flag only");
    assert_eq!(flag_only.fullscreen_intent(), FullscreenIntent::Enter);
    assert!(flag_only.initial_media_arguments().is_empty());
}

/// После `--` строка `--fullscreen` — имя файла, а не опция.
#[test]
fn fullscreen_after_the_delimiter_is_a_file_name() {
    let parsed = ProcessArgs::parse(os_strings(&["--", "--fullscreen"])).expect("file name");

    assert_eq!(parsed.fullscreen_intent(), FullscreenIntent::LeaveAsIs);
    assert_eq!(
        parsed.initial_media_arguments(),
        os_strings(&["--fullscreen"])
    );
}

/// Знакомая опция не открывает дорогу похожим: `--fs`, `--fullscreen=1` — неизвестные.
#[test]
fn other_options_stay_unknown_next_to_fullscreen() {
    for arguments in [
        &["--fullscreen", "--fs"][..],
        &["--fullscreen=1", "a.mkv"][..],
        &["-f", "a.mkv"][..],
    ] {
        assert_eq!(
            ProcessArgs::parse(os_strings(arguments)),
            Err(ProcessArgsError::UnknownOption),
            "{arguments:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn process_args_preserves_non_utf8_local_paths_exactly() {
    use std::os::unix::ffi::OsStringExt;

    let first = OsString::from_vec(b"movie-\xFF.mkv".to_vec());
    let second = OsString::from_vec(b"\xFE\xFDclip.mkv".to_vec());
    let mut parsed =
        ProcessArgs::parse([first.clone(), second.clone()]).expect("native local paths");

    assert_eq!(parsed.take_initial_media_arguments(), vec![first, second]);
}

#[test]
fn bootstrap_calls_discover_lease_load_and_prepare_in_exact_order() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let platform = FakePlatform {
        calls: calls.clone(),
        outcome: Ok(()),
    };

    let result = bootstrap_with(
        [OsString::from("movie.mkv")],
        {
            let calls = calls.clone();
            move || {
                calls.borrow_mut().push("discover-paths");
                Ok(ConfigPaths::from_config_dir("/explicit/test-root"))
            }
        },
        &platform,
        {
            let calls = calls.clone();
            move |_| {
                calls.borrow_mut().push("load-config");
                Ok(())
            }
        },
        {
            let calls = calls.clone();
            move |_, _, _| calls.borrow_mut().push("prepare-media")
        },
        |_| panic!("Lease Received: Transfers Should Not Be"),
    );

    assert!(result.is_ok());
    assert_eq!(
        *calls.borrow(),
        [
            "discover-paths",
            "acquire-lease",
            "load-config",
            "prepare-media"
        ]
    );
}

#[test]
fn invalid_args_stop_before_path_discovery() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let platform = FakePlatform {
        calls: calls.clone(),
        outcome: Ok(()),
    };
    let result = bootstrap_with(
        [OsString::from("--unknown")],
        {
            let calls = calls.clone();
            move || {
                calls.borrow_mut().push("discover-paths");
                Ok(ConfigPaths::from_config_dir("/unused"))
            }
        },
        &platform,
        |_| Ok(()),
        |_, _, _| (),
        |_| panic!("There should be no forwarding."),
    );

    assert!(matches!(
        result,
        Err(ProcessBootstrapError::Arguments(
            ProcessArgsError::UnknownOption
        ))
    ));
    assert!(calls.borrow().is_empty());
}

#[test]
fn unsupported_platform_stops_before_post_lease_side_effects() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let platform = FakePlatform {
        calls: calls.clone(),
        outcome: Err(AppInstanceLeaseError::UnsupportedPlatform),
    };
    let result = bootstrap_with(
        Vec::<OsString>::new(),
        {
            let calls = calls.clone();
            move || {
                calls.borrow_mut().push("discover-paths");
                Ok(ConfigPaths::from_config_dir("/explicit/test-root"))
            }
        },
        &platform,
        {
            let calls = calls.clone();
            move |_| {
                calls.borrow_mut().push("load-config");
                Ok(())
            }
        },
        {
            let calls = calls.clone();
            move |_, _, _| calls.borrow_mut().push("prepare-media")
        },
        |_| panic!("Lease Received: Transfers Should Not Be"),
    );

    assert!(matches!(
        result,
        Err(ProcessBootstrapError::Lease {
            lease_error: AppInstanceLeaseError::UnsupportedPlatform,
            ..
        })
    ));
    assert_eq!(*calls.borrow(), ["discover-paths", "acquire-lease"]);
}

/// Сквозной путь: ошибка bootstrap → текст для пользователя (через fake-показ) →
/// ненулевой код выхода. Проверяет и то, что до config дело не доходит.
#[test]
fn bootstrap_failures_reach_user_as_human_text_with_nonzero_exit() {
    let cases: [(Vec<OsString>, Result<(), AppInstanceLeaseError>, &str); 3] = [
        (vec![OsString::from("--unknown")], Ok(()), "unknown option"),
        (
            Vec::new(),
            Err(AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::ConfigDirectoryOwnerMismatch,
            }),
            "sudo chown -R \"$USER\": /explicit/test-root",
        ),
        (
            Vec::new(),
            Err(AppInstanceLeaseError::AlreadyRunning),
            "Fastiplayer is already running.",
        ),
    ];

    for (arguments, lease_outcome, expected_text) in cases {
        let config_loaded = Rc::new(RefCell::new(false));
        let platform = FakePlatform {
            calls: Rc::new(RefCell::new(Vec::new())),
            outcome: lease_outcome,
        };
        let bootstrap_error = bootstrap_with(
            arguments,
            || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
            &platform,
            {
                let config_loaded = config_loaded.clone();
                move |_| {
                    *config_loaded.borrow_mut() = true;
                    Ok(())
                }
            },
            |_, _, _| (),
            // Второй запуск без файлов, первый экземпляр так и не начал слушать.
            |_| {
                Err(RunningInstanceForwardingError {
                    failure: RunningInstanceForwardingFailure::Delivery(
                        InstanceForwardingError::InstanceNotListening,
                    ),
                    payload: ForwardedPayload::WindowActivationOnly,
                })
            },
        )
        .err()
        .expect("bootstrap must fail");
        let presenter = RecordingPresenter::default();

        let conclusion = conclude_process(
            Err(FatalStartupError::from_bootstrap_error(&bootstrap_error)),
            &presenter,
        );

        let shown = presenter.shown_notices();
        assert_eq!(conclusion, ProcessConclusion::FailedToStart);
        assert_eq!(shown.len(), 1);
        assert!(
            shown[0].notice.message.contains(expected_text),
            "{}",
            shown[0].notice.message
        );
        assert!(!*config_loaded.borrow());
    }
}

#[test]
fn fake_platform_errors_keep_shared_typed_mapping() {
    // `AlreadyRunning` сюда не входит: он ведёт к пересылке (отдельные тесты ниже).
    let expected_errors = [
        AppInstanceLeaseError::Io {
            operation: AppInstanceLeaseIoOperation::AcquireLock,
            kind: std::io::ErrorKind::PermissionDenied,
        },
        AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::LockArtifactOwnerMismatch,
        },
        AppInstanceLeaseError::UnsupportedPlatform,
    ];

    for expected_error in expected_errors {
        let platform = FakePlatform {
            calls: Rc::new(RefCell::new(Vec::new())),
            outcome: Err(expected_error),
        };
        let result = bootstrap_with(
            Vec::<OsString>::new(),
            || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
            &platform,
            |_| Ok(()),
            |_, _, _| (),
            |_| panic!("There should be no forwarding."),
        );

        assert!(matches!(
            result,
            Err(ProcessBootstrapError::Lease { lease_error, ref config_dir })
                if lease_error == expected_error
                    && config_dir == std::path::Path::new("/explicit/test-root")
        ));
    }
}

/// Второй запуск: lease занят → аргументы уходят пересылке ровно такими, как
/// пришли, config этого процесса не читается, media не классифицируется.
#[test]
fn already_running_forwards_untouched_arguments_and_never_loads_config() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let forwarded_arguments = Rc::new(RefCell::new(Vec::new()));
    let platform = FakePlatform {
        calls: calls.clone(),
        outcome: Err(AppInstanceLeaseError::AlreadyRunning),
    };

    let outcome = bootstrap_with(
        [
            OsString::from("b.mkv"),
            OsString::from("--"),
            OsString::from("-a.mkv"),
        ],
        || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
        &platform,
        {
            let calls = calls.clone();
            move |_| {
                calls.borrow_mut().push("load-config");
                Ok(())
            }
        },
        {
            let calls = calls.clone();
            move |_, _, _| calls.borrow_mut().push("prepare-media")
        },
        {
            let calls = calls.clone();
            let forwarded_arguments = forwarded_arguments.clone();
            move |process_args| {
                calls.borrow_mut().push("forward");
                *forwarded_arguments.borrow_mut() = process_args.initial_media_arguments().to_vec();
                Ok(())
            }
        },
    )
    .expect("forwarding");

    assert!(matches!(
        outcome,
        BootstrapOutcome::ForwardedToRunningInstance
    ));
    assert_eq!(*calls.borrow(), ["acquire-lease", "forward"]);
    assert_eq!(
        *forwarded_arguments.borrow(),
        [OsString::from("b.mkv"), OsString::from("-a.mkv")]
    );
}

/// Второй запуск с `--fullscreen`: намерение доходит до пересылки вместе с файлами
/// (сама опция среди файлов не пересылается).
#[test]
fn already_running_forwards_the_fullscreen_intent_with_files() {
    let forwarded = Rc::new(RefCell::new(None));
    let platform = FakePlatform {
        calls: Rc::new(RefCell::new(Vec::new())),
        outcome: Err(AppInstanceLeaseError::AlreadyRunning),
    };

    bootstrap_with(
        os_strings(&["--fullscreen", "film.mkv"]),
        || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
        &platform,
        |_| Ok(()),
        |_, _, _| (),
        {
            let forwarded = forwarded.clone();
            move |process_args| {
                *forwarded.borrow_mut() = Some((
                    process_args.initial_media_arguments().to_vec(),
                    process_args.fullscreen_intent(),
                ));
                Ok(())
            }
        },
    )
    .expect("forwarding");

    assert_eq!(
        *forwarded.borrow(),
        Some((os_strings(&["film.mkv"]), FullscreenIntent::Enter))
    );
}

/// Сквозной путь второго запуска: успех → ничего не показано, код выхода 0;
/// первый экземпляр завис → понятный текст и ненулевой код, config не тронут.
#[test]
fn second_launch_exits_cleanly_on_success_and_explains_a_hung_first_instance() {
    let presenter = RecordingPresenter::default();
    let forwarded = bootstrap_with(
        [OsString::from("/videos/a.mkv")],
        || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
        &FakePlatform {
            calls: Rc::new(RefCell::new(Vec::new())),
            outcome: Err(AppInstanceLeaseError::AlreadyRunning),
        },
        |_| -> Result<(), fastiplayer_config::ConfigError> {
            panic!("Config of the second process is not readable")
        },
        |_, _, _| (),
        |_| Ok(()),
    );
    assert!(matches!(
        forwarded,
        Ok(BootstrapOutcome::ForwardedToRunningInstance)
    ));
    assert_eq!(
        conclude_process(Ok(()), &presenter),
        ProcessConclusion::Completed
    );
    assert!(presenter.shown_notices().is_empty());

    let hung = bootstrap_with(
        [OsString::from("/videos/a.mkv")],
        || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
        &FakePlatform {
            calls: Rc::new(RefCell::new(Vec::new())),
            outcome: Err(AppInstanceLeaseError::AlreadyRunning),
        },
        |_| -> Result<(), fastiplayer_config::ConfigError> {
            panic!("Config of the second process is not readable")
        },
        |_, _, _| (),
        |_| {
            Err(RunningInstanceForwardingError {
                failure: RunningInstanceForwardingFailure::Delivery(
                    InstanceForwardingError::InstanceNotResponding,
                ),
                payload: ForwardedPayload::MediaArguments,
            })
        },
    )
    .err()
    .expect("forwarding failed");
    let conclusion = conclude_process(
        Err(FatalStartupError::from_bootstrap_error(&hung)),
        &presenter,
    );

    assert_eq!(conclusion, ProcessConclusion::FailedToStart);
    let shown = presenter.shown_notices();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0]
            .notice
            .message
            .starts_with("Fastiplayer is already running but is not responding."),
        "{}",
        shown[0].notice.message
    );
}
