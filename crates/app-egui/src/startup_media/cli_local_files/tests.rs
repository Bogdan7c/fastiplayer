use std::path::PathBuf;

use playlist_discovery::LocalMediaKind;

use super::*;

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn single_cli_file_keeps_sibling_discovery_after_install() {
    let mut orchestration = StartupMediaOrchestration::new(true);

    assert!(matches!(
        orchestration.local_install_follow_up(true, LocalMediaKind::VideoContaining),
        StartupSiblingDiscovery::AfterInstall(LocalMediaKind::VideoContaining)
    ));
}

#[test]
fn cli_file_set_appends_rest_in_given_order_instead_of_sibling_discovery() {
    let mut orchestration = StartupMediaOrchestration::new(true);
    orchestration.hold_cli_follow_up_files(paths(&["/v/z.mkv", "/v/a.mkv"]));

    let follow_up = orchestration.local_install_follow_up(true, LocalMediaKind::AudioOnly);

    assert!(matches!(
        follow_up,
        StartupSiblingDiscovery::AppendCliFollowUpFiles(files)
            if files == paths(&["/v/z.mkv", "/v/a.mkv"])
    ));
    // Хвост забирается ровно один раз.
    assert!(matches!(
        orchestration.local_install_follow_up(true, LocalMediaKind::AudioOnly),
        StartupSiblingDiscovery::AfterInstall(LocalMediaKind::AudioOnly)
    ));
}

#[test]
fn restored_fallback_install_neither_searches_siblings_nor_takes_cli_tail() {
    let mut orchestration = StartupMediaOrchestration::new(true);
    orchestration.hold_cli_follow_up_files(paths(&["/v/second.mkv"]));

    assert!(matches!(
        orchestration.local_install_follow_up(false, LocalMediaKind::VideoContaining),
        StartupSiblingDiscovery::Skip
    ));
    // Восстановленный элемент не «съел» хвост CLI-набора.
    assert!(matches!(
        orchestration.local_install_follow_up(true, LocalMediaKind::VideoContaining),
        StartupSiblingDiscovery::AppendCliFollowUpFiles(files)
            if files == paths(&["/v/second.mkv"])
    ));
}
