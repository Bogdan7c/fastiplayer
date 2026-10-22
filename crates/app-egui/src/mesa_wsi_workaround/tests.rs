use std::ffi::{OsStr, OsString};

use super::*;

fn written(value: &str, outcome: MesaCommitTimingWorkaround) -> MesaWsiDebugUpdate {
    MesaWsiDebugUpdate::Write {
        value: OsString::from(value),
        outcome,
    }
}

#[test]
fn absent_variable_gets_only_the_commit_timing_flag() {
    assert_eq!(
        mesa_wsi_debug_with_commit_timing_disabled(None),
        written("nowlts", MesaCommitTimingWorkaround::Enabled)
    );
}

#[test]
fn empty_variable_is_treated_as_absent() {
    assert_eq!(
        mesa_wsi_debug_with_commit_timing_disabled(Some(OsStr::new(""))),
        written("nowlts", MesaCommitTimingWorkaround::Enabled)
    );
}

#[test]
fn user_flags_are_kept_and_the_flag_is_appended() {
    assert_eq!(
        mesa_wsi_debug_with_commit_timing_disabled(Some(OsStr::new("noshm,linear"))),
        written(
            "noshm,linear,nowlts",
            MesaCommitTimingWorkaround::AppendedToUserFlags
        )
    );
}

#[test]
fn flag_already_listed_by_comma_or_space_leaves_environment_untouched() {
    for listed in ["nowlts", "noshm,nowlts", "nowlts linear"] {
        assert_eq!(
            mesa_wsi_debug_with_commit_timing_disabled(Some(OsStr::new(listed))),
            MesaWsiDebugUpdate::Keep,
            "{listed}"
        );
    }
}

#[test]
fn similar_word_is_not_mistaken_for_the_flag() {
    assert_eq!(
        mesa_wsi_debug_with_commit_timing_disabled(Some(OsStr::new("nowltsx"))),
        written(
            "nowltsx,nowlts",
            MesaCommitTimingWorkaround::AppendedToUserFlags
        )
    );
}

#[test]
fn non_utf8_user_value_keeps_its_original_bytes() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let user_value = OsStr::from_bytes(b"no\xffshm");
    let MesaWsiDebugUpdate::Write { value, outcome } =
        mesa_wsi_debug_with_commit_timing_disabled(Some(user_value))
    else {
        panic!("флаг должен быть дописан");
    };

    assert_eq!(value.into_vec(), b"no\xffshm,nowlts".to_vec());
    assert_eq!(outcome, MesaCommitTimingWorkaround::AppendedToUserFlags);
}
