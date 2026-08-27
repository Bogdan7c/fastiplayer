use super::event_loop::should_request_redraw_for_wake;

#[test]
fn idle_and_noop_wakes_never_request_redraw() {
    assert!(!should_request_redraw_for_wake(true, false));
    assert!(!should_request_redraw_for_wake(false, false));
}

#[test]
fn visible_wake_requires_live_window() {
    assert!(should_request_redraw_for_wake(true, true));
    assert!(!should_request_redraw_for_wake(false, true));
}

#[test]
fn desktop_backend_start_is_inside_lease_owning_shell_constructor() {
    let shell_source = include_str!("mod.rs");
    let lease_argument = shell_source
        .find("instance_lease: AppInstanceLease")
        .expect("AppShell constructor requires an acquired lease");
    let desktop_start = shell_source
        .find("playlist_runtime.start_desktop_transport(")
        .expect("desktop backend starts from process shell");
    let retained_lease = shell_source
        .find("_instance_lease: instance_lease")
        .expect("process shell retains lease for its full lifetime");
    assert!(lease_argument < desktop_start && desktop_start < retained_lease);
}

#[test]
fn suspend_and_terminal_exit_force_flush_pending_sidebar_resize() {
    let shell_source = include_str!("mod.rs");
    let flush_call = format!(
        "{}{}",
        "self.flush_sidebar_resize_for_lifecycle_boundary", "();"
    );
    assert_eq!(
        shell_source.matches(&flush_call).count(),
        2,
        "suspend и terminal shutdown должны flush-ить resize до уничтожения AppState"
    );

    let suspend_start = shell_source
        .find("fn suspend_runtime(&mut self)")
        .expect("suspend lifecycle method exists");
    let suspend_flush = shell_source[suspend_start..]
        .find(&flush_call)
        .expect("suspend flush exists");
    let suspend_drop = shell_source[suspend_start..]
        .find("self.app_state = None;")
        .expect("suspend drops AppState");
    assert!(suspend_flush < suspend_drop);

    let shutdown_start = shell_source
        .find("pub(crate) fn finish_process_shutdown(&mut self)")
        .expect("terminal shutdown method exists");
    let shutdown_flush = shell_source[shutdown_start..]
        .find(&flush_call)
        .expect("terminal flush exists");
    let shutdown_drop = shell_source[shutdown_start..]
        .find("self.app_state = None;")
        .expect("terminal shutdown drops AppState");
    assert!(shutdown_flush < shutdown_drop);
}

#[test]
fn window_creation_keeps_minimum_logical_width_at_four_hundred_points() {
    let event_loop_source = include_str!("event_loop.rs");
    let creation = event_loop_source
        .split_once("let window_attributes = Window::default_attributes()")
        .expect("window attributes construction must exist")
        .1
        .split_once("let window = match")
        .expect("window attributes construction must stay bounded")
        .0;

    assert!(creation.contains("with_min_inner_size"));
    assert!(creation.contains("LogicalSize::new(400.0, 1.0)"));
}
