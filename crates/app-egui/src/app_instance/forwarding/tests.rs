use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use desktop_integration::{
    ForwardedInstanceAction, ForwardedInstanceRequest, ForwardedRequestRejection,
    InstanceForwardingError, MAX_FORWARDED_URIS, WindowActivationToken,
};

use super::{
    ForwardedPayload, ForwardingEnvironment, RunningInstanceForwarder,
    RunningInstanceForwardingFailure, activation_token_from, forward_arguments_to_running_instance,
};
use crate::external_open::request::ExternalOpenItem;
use crate::external_open::uri::item_from_uri;

/// Поддельный транспорт: запоминает запрос и отвечает заданным исходом.
struct RecordingForwarder {
    outcome: Result<(), InstanceForwardingError>,
    sent: RefCell<Vec<ForwardedInstanceRequest>>,
}

impl RecordingForwarder {
    fn answering(outcome: Result<(), InstanceForwardingError>) -> Self {
        Self {
            outcome,
            sent: RefCell::new(Vec::new()),
        }
    }

    fn single_sent(&self) -> ForwardedInstanceRequest {
        let sent = self.sent.borrow();
        assert_eq!(sent.len(), 1, "ожидался ровно один запрос");
        sent[0].clone()
    }
}

impl RunningInstanceForwarder for RecordingForwarder {
    fn forward(&self, request: &ForwardedInstanceRequest) -> Result<(), InstanceForwardingError> {
        self.sent.borrow_mut().push(request.clone());
        self.outcome.clone()
    }
}

fn environment_in(directory: &str) -> ForwardingEnvironment {
    ForwardingEnvironment {
        current_directory: Some(PathBuf::from(directory)),
        activation_token: None,
    }
}

fn sent_uris(request: &ForwardedInstanceRequest) -> Vec<String> {
    match request.action() {
        ForwardedInstanceAction::Open(uris) => {
            uris.iter().map(|uri| uri.as_str().to_owned()).collect()
        }
        ForwardedInstanceAction::Activate => panic!("ожидалось Open"),
    }
}

/// Главный сценарий: получатель раскодирует пересланный URI в тот же путь,
/// который второй процесс получил (с учётом его рабочей папки).
#[cfg(unix)]
#[test]
fn local_paths_arrive_at_receiver_as_the_same_absolute_paths() {
    use std::os::unix::ffi::OsStringExt;

    let non_utf8_name = OsString::from_vec(b"\xFFclip.mkv".to_vec());
    let arguments = vec![
        OsString::from("/videos/абсолютный путь.mkv"),
        OsString::from("relative/b.mkv"),
        non_utf8_name.clone(),
    ];
    let forwarder = RecordingForwarder::answering(Ok(()));

    forward_arguments_to_running_instance(
        &arguments,
        environment_in("/home/u/Загрузки"),
        &forwarder,
    )
    .expect("пересылка удалась");

    let received_paths: Vec<ExternalOpenItem> = sent_uris(&forwarder.single_sent())
        .iter()
        .map(|uri| item_from_uri(uri))
        .collect();
    assert_eq!(
        received_paths,
        [
            ExternalOpenItem::LocalPath(PathBuf::from("/videos/абсолютный путь.mkv")),
            ExternalOpenItem::LocalPath(PathBuf::from("/home/u/Загрузки/relative/b.mkv")),
            ExternalOpenItem::LocalPath(Path::new("/home/u/Загрузки").join(non_utf8_name)),
        ]
    );
}

#[test]
fn links_are_forwarded_verbatim_and_keep_their_order_with_files() {
    let arguments = vec![
        OsString::from("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=5"),
        OsString::from("/videos/a.mkv"),
    ];
    let forwarder = RecordingForwarder::answering(Ok(()));

    forward_arguments_to_running_instance(&arguments, environment_in("/"), &forwarder)
        .expect("пересылка удалась");

    let uris = sent_uris(&forwarder.single_sent());
    assert_eq!(uris[0], "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=5");
    assert_eq!(uris[1], "file:///videos/a.mkv");
}

#[test]
fn launch_without_arguments_only_activates_the_window_with_the_desktop_token() {
    let forwarder = RecordingForwarder::answering(Ok(()));
    let token = WindowActivationToken::from_raw("kwin-42".to_owned()).expect("билет");

    forward_arguments_to_running_instance(
        &[],
        ForwardingEnvironment {
            current_directory: None,
            activation_token: Some(token.clone()),
        },
        &forwarder,
    )
    .expect("пересылка удалась");

    let sent = forwarder.single_sent();
    assert_eq!(sent.action(), &ForwardedInstanceAction::Activate);
    assert_eq!(sent.activation_token(), Some(&token));
}

#[test]
fn relative_path_without_working_directory_is_an_error_not_a_wrong_file() {
    let forwarder = RecordingForwarder::answering(Ok(()));

    let error = forward_arguments_to_running_instance(
        &[OsString::from("movie.mkv")],
        ForwardingEnvironment {
            current_directory: None,
            activation_token: None,
        },
        &forwarder,
    )
    .expect_err("путь не к чему привязать");

    assert_eq!(
        error.failure,
        RunningInstanceForwardingFailure::CurrentDirectoryUnavailable
    );
    assert_eq!(error.payload, ForwardedPayload::MediaArguments);
    assert!(forwarder.sent.borrow().is_empty(), "ничего не отправлено");
}

#[test]
fn too_many_files_are_refused_before_touching_the_bus() {
    let forwarder = RecordingForwarder::answering(Ok(()));
    let arguments = vec![OsString::from("/videos/a.mkv"); MAX_FORWARDED_URIS + 1];

    let error = forward_arguments_to_running_instance(&arguments, environment_in("/"), &forwarder)
        .expect_err("лимит протокола");

    assert_eq!(
        error.failure,
        RunningInstanceForwardingFailure::RequestNotForwardable(
            ForwardedRequestRejection::TooManyUris {
                count: MAX_FORWARDED_URIS + 1
            }
        )
    );
    assert!(forwarder.sent.borrow().is_empty());
}

#[test]
fn transport_failure_keeps_its_reason_and_what_was_forwarded() {
    for (arguments, expected_payload) in [
        (Vec::new(), ForwardedPayload::WindowActivationOnly),
        (
            vec![OsString::from("/a.mkv")],
            ForwardedPayload::MediaArguments,
        ),
    ] {
        let forwarder =
            RecordingForwarder::answering(Err(InstanceForwardingError::InstanceNotResponding));

        let error =
            forward_arguments_to_running_instance(&arguments, environment_in("/"), &forwarder)
                .expect_err("транспорт отказал");

        assert_eq!(
            error.failure,
            RunningInstanceForwardingFailure::Delivery(
                InstanceForwardingError::InstanceNotResponding
            )
        );
        assert_eq!(error.payload, expected_payload);
    }
}

#[test]
fn wayland_token_wins_over_x11_and_garbage_tokens_are_skipped() {
    let both = |variable: &str| match variable {
        "XDG_ACTIVATION_TOKEN" => Some("wayland-token".to_owned()),
        "DESKTOP_STARTUP_ID" => Some("x11-token".to_owned()),
        _ => None,
    };
    assert_eq!(
        activation_token_from(both).map(WindowActivationToken::into_string),
        Some("wayland-token".to_owned())
    );

    let broken_wayland = |variable: &str| match variable {
        "XDG_ACTIVATION_TOKEN" => Some("не билет".to_owned()),
        "DESKTOP_STARTUP_ID" => Some("x11-token".to_owned()),
        _ => None,
    };
    assert_eq!(
        activation_token_from(broken_wayland).map(WindowActivationToken::into_string),
        Some("x11-token".to_owned())
    );

    assert_eq!(activation_token_from(|_| None), None);
}
