use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::model::{ProgressWindowCloseDecisionDto, RunPurpose};
use super::progress_window::{self, ProgressWindowSurface};
use super::RunLifecycle;

/// Stands in for the progress webview. Whether an abandoned handshake leaves a window on screen,
/// and whether it leaves the lifecycle able to start another run, is decided in `progress_window`
/// and has to be observable without a webview.
#[derive(Default)]
struct FakeProgressWindow {
    destroyed: AtomicBool,
}

impl FakeProgressWindow {
    fn destroyed(&self) -> bool {
        self.destroyed.load(Ordering::Relaxed)
    }
}

impl ProgressWindowSurface for FakeProgressWindow {
    fn emit_arm(&self, _launch_id: u64) -> Result<(), String> {
        Ok(())
    }

    fn destroy_window(&self) -> Result<(), String> {
        self.destroyed.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn tearing_the_window_down_mid_mount_settles_a_close_the_webview_can_no_longer_finish() {
    let lifecycle = RunLifecycle::default();
    let launch_id = lifecycle.reserve_progress_launch().unwrap();
    let mounted_rx = lifecycle.prepare_progress_window_mount(launch_id).unwrap();
    let window = FakeProgressWindow::default();

    // The progress webview asks to close while preparation is still waiting for the mount signal.
    // Taking the pending launch drops the mount sender under the lock, so the wait is released at
    // once and preparation reaches its teardown ahead of the webview's own destroy request.
    assert_eq!(
        lifecycle.begin_progress_window_close(),
        ProgressWindowCloseDecisionDto::PendingLaunchCancelled
    );

    let error = tauri::async_runtime::block_on(progress_window::arm(
        &window,
        &lifecycle,
        launch_id,
        Some(mounted_rx),
    ))
    .unwrap_err();
    assert!(error.contains("did not load"), "{error}");
    assert!(window.destroyed());

    // Destroying the window is what ends the close, whoever destroys it. Only the progress webview
    // may call `destroy_progress_window`, so a teardown here that does not settle the close leaves
    // `reserve_progress_launch` refusing every later Apply for the life of the process.
    lifecycle
        .reserve_progress_launch()
        .expect("a backend teardown must settle the progress-window close");
}

#[test]
fn a_launch_withdrawn_between_mount_and_arm_leaves_no_window_and_no_stranded_close() {
    let lifecycle = RunLifecycle::default();
    let launch_id = lifecycle.reserve_progress_launch().unwrap();
    let mounted_rx = lifecycle.prepare_progress_window_mount(launch_id).unwrap();
    lifecycle.report_progress_window_mounted(launch_id).unwrap();
    let window = FakeProgressWindow::default();

    // The webview reported mounted and is then closed, in the gap before preparation arms it.
    assert_eq!(
        lifecycle.begin_progress_window_close(),
        ProgressWindowCloseDecisionDto::PendingLaunchCancelled
    );

    let error = tauri::async_runtime::block_on(progress_window::arm(
        &window,
        &lifecycle,
        launch_id,
        Some(mounted_rx),
    ))
    .unwrap_err();
    assert!(error.contains("no longer active"), "{error}");
    assert!(
        window.destroyed(),
        "a window that never armed must not stay on screen"
    );
    lifecycle
        .reserve_progress_launch()
        .expect("a backend teardown must settle the progress-window close");
}

#[test]
fn progress_launch_is_reserved_and_consumed_exactly_once() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let launch_id = lifecycle.reserve_progress_launch().unwrap();
    assert!(lifecycle.reserve_progress_launch().is_err());
    let command = lifecycle.command_lease().unwrap();
    assert!(command.start_run(RunPurpose::Compare).is_err());
    assert!(command
        .start_apply_from_progress_launch(launch_id + 1)
        .is_err());
    assert!(command.start_apply_from_progress_launch(launch_id).is_err());

    let ready = lifecycle
        .prepare_progress_launch_acknowledgement(launch_id)
        .unwrap();
    lifecycle.acknowledge_progress_launch(launch_id).unwrap();
    ready.recv().unwrap();

    let run = command.start_apply_from_progress_launch(launch_id).unwrap();
    assert_eq!(run.run_id(), 1);
    assert_eq!(
        lifecycle.begin_progress_window_close(),
        ProgressWindowCloseDecisionDto::ActiveRunCancellationRequested { run_id: 1 }
    );
    assert!(run.control().cancelled());
    drop(run);
    drop(command);
    assert!(!lifecycle.has_activity());
}

#[test]
fn closing_window_blocks_new_launches_until_destruction_finishes() {
    let lifecycle = RunLifecycle::default();
    assert_eq!(
        lifecycle.begin_progress_window_close(),
        ProgressWindowCloseDecisionDto::NoInteractiveLaunch
    );
    assert!(lifecycle.reserve_progress_launch().is_err());
    lifecycle.finish_progress_window_close();
    assert!(lifecycle.reserve_progress_launch().is_ok());
}

#[test]
fn pending_launch_close_invalidates_only_that_launch() {
    let lifecycle = RunLifecycle::default();
    let launch_id = lifecycle.reserve_progress_launch().unwrap();
    assert!(!lifecycle.cancel_progress_launch(launch_id + 1));
    assert_eq!(
        lifecycle.begin_progress_window_close(),
        ProgressWindowCloseDecisionDto::PendingLaunchCancelled
    );
    assert!(!lifecycle.cancel_progress_launch(launch_id));
}

#[test]
fn closing_an_open_progress_window_cancels_an_unattended_apply() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let command = lifecycle.command_lease().unwrap();
    let run = command.start_run(RunPurpose::Apply).unwrap();

    assert_eq!(
        lifecycle.begin_progress_window_close(),
        ProgressWindowCloseDecisionDto::ActiveRunCancellationRequested { run_id: 1 }
    );
    assert!(run.control().cancelled());
}

#[test]
fn delayed_controls_cannot_target_a_newer_run() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let command = lifecycle.command_lease().unwrap();
    let first = command.start_run(RunPurpose::Compare).unwrap();
    let first_id = first.run_id();
    drop(first);
    let second = command.start_run(RunPurpose::Apply).unwrap();
    let second_id = second.run_id();

    assert!(lifecycle
        .request_cancel(first_id, RunPurpose::Compare)
        .is_err());
    assert!(lifecycle
        .set_paused(first_id, RunPurpose::Apply, true)
        .is_err());
    assert!(lifecycle
        .request_cancel(second_id, RunPurpose::Compare)
        .is_err());
    assert!(lifecycle
        .set_paused(second_id, RunPurpose::Apply, true)
        .unwrap());
    assert!(lifecycle
        .request_cancel(second_id, RunPurpose::Apply)
        .unwrap());
    assert!(second.control().cancelled());
}

#[test]
fn progress_handshake_binds_mount_and_readiness_to_one_launch() {
    let lifecycle = RunLifecycle::default();
    let launch_id = lifecycle.reserve_progress_launch().unwrap();
    assert!(lifecycle.report_progress_window_mounted(launch_id).is_err());

    let mounted = lifecycle.prepare_progress_window_mount(launch_id).unwrap();
    assert!(lifecycle
        .report_progress_window_mounted(launch_id + 1)
        .is_err());
    lifecycle.report_progress_window_mounted(launch_id).unwrap();
    mounted.recv().unwrap();

    let ready = lifecycle
        .prepare_progress_launch_acknowledgement(launch_id)
        .unwrap();
    assert!(lifecycle
        .acknowledge_progress_launch(launch_id + 1)
        .is_err());
    lifecycle.acknowledge_progress_launch(launch_id).unwrap();
    ready.recv().unwrap();
    assert!(lifecycle.acknowledge_progress_launch(launch_id).is_err());
}

#[test]
fn command_and_active_run_leases_release_during_unwind() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
        let lifecycle = lifecycle.clone();
        move || {
            let command = lifecycle.command_lease().unwrap();
            let _run = command.start_run(RunPurpose::Compare).unwrap();
            panic!("worker panic");
        }
    }));

    assert!(result.is_err());
    assert!(!lifecycle.has_activity());
    let command = lifecycle.command_lease().unwrap();
    assert!(command.start_run(RunPurpose::Compare).is_ok());
}

#[test]
fn idle_mutation_excludes_every_command_and_run_state() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let command = lifecycle.command_lease().unwrap();
    let error = lifecycle
        .with_idle_mutation("Saving jobs", || Ok(()))
        .unwrap_err();
    assert!(error.contains("unavailable"), "{error}");
    drop(command);

    assert_eq!(
        lifecycle
            .with_idle_mutation("Saving jobs", || {
                assert!(lifecycle.command_lease().is_err());
                assert!(lifecycle.reserve_progress_launch().is_err());
                Ok(42)
            })
            .unwrap(),
        42
    );
    assert!(lifecycle.command_lease().is_ok());
}

#[test]
fn preparing_command_blocks_progress_launch_reservation() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let command = lifecycle.command_lease().unwrap();
    assert!(lifecycle.reserve_progress_launch().is_err());
    drop(command);
    assert!(lifecycle.reserve_progress_launch().is_ok());
}

#[test]
fn successful_apply_grant_is_exact_retryable_and_one_use() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let command = lifecycle.command_lease().unwrap();
    let run = command.start_run(RunPurpose::Apply).unwrap();
    let run_id = run.run_id();
    lifecycle.issue_post_run_power_action_grant(run_id).unwrap();
    drop(run);
    drop(command);

    assert!(lifecycle
        .consume_post_run_power_action_grant_with(run_id + 1, || Ok(()))
        .is_err());
    assert!(lifecycle
        .consume_post_run_power_action_grant_with::<()>(run_id, || {
            Err("system command unavailable".into())
        })
        .is_err());
    assert!(lifecycle
        .consume_post_run_power_action_grant_with(run_id, || Ok(()))
        .is_ok());
    assert!(lifecycle
        .consume_post_run_power_action_grant_with(run_id, || Ok(()))
        .is_err());
}

#[test]
fn compare_and_a_new_launch_cannot_reuse_a_power_action_grant() {
    let lifecycle = Arc::new(RunLifecycle::default());
    let command = lifecycle.command_lease().unwrap();
    let compare = command.start_run(RunPurpose::Compare).unwrap();
    assert!(lifecycle
        .issue_post_run_power_action_grant(compare.run_id())
        .is_err());
    drop(compare);
    let apply = command.start_run(RunPurpose::Apply).unwrap();
    let apply_id = apply.run_id();
    lifecycle
        .issue_post_run_power_action_grant(apply_id)
        .unwrap();
    drop(apply);
    drop(command);

    lifecycle.reserve_progress_launch().unwrap();
    assert!(lifecycle
        .consume_post_run_power_action_grant_with(apply_id, || Ok(()))
        .is_err());
}
