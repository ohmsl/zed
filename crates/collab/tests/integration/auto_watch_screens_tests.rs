use crate::TestServer;
use call::ActiveCall;
use gpui::{BackgroundExecutor, Entity, TestAppContext, TestScreenCaptureSource};
use project::Project;
use serde_json::json;
use util::path;
use workspace::{Item as _, SharedScreen, item::ItemHandle as _};

use super::TestClient;

async fn start_screen_share(cx: &mut TestAppContext) {
    let display = TestScreenCaptureSource::new();
    cx.set_screen_capture_sources(vec![display]);
    let screen = cx
        .update(|cx| cx.screen_capture_sources())
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let active_call = cx.read(ActiveCall::global);
    active_call
        .update(cx, |call, cx| {
            call.room()
                .unwrap()
                .update(cx, |room, cx| room.share_screen(screen, cx))
        })
        .await
        .unwrap();
}

fn stop_screen_share(cx: &mut TestAppContext) {
    let active_call = cx.read(ActiveCall::global);
    active_call
        .update(cx, |call, cx| {
            call.room()
                .unwrap()
                .update(cx, |room, cx| room.unshare_screen(true, cx))
        })
        .unwrap();
}

struct AutoWatchTestSetup {
    client_a: TestClient,
    _client_b: TestClient,
    _client_c: TestClient,
    project_a: Entity<Project>,
}

async fn setup_auto_watch_test(
    server: &mut TestServer,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) -> AutoWatchTestSetup {
    let client_a = server.create_client(cx_a, "user_a").await;
    let client_b = server.create_client(cx_b, "user_b").await;
    let client_c = server.create_client(cx_c, "user_c").await;
    server
        .create_room(&mut [(&client_a, cx_a), (&client_b, cx_b), (&client_c, cx_c)])
        .await;

    let active_call_a = cx_a.read(ActiveCall::global);

    client_a
        .fs()
        .insert_tree(path!("/a"), json!({ "file.txt": "content" }))
        .await;
    let (project_a, _worktree_id) = client_a.build_local_project(path!("/a"), cx_a).await;
    active_call_a
        .update(cx_a, |call, cx| call.set_location(Some(&project_a), cx))
        .await
        .unwrap();

    AutoWatchTestSetup {
        client_a,
        _client_b: client_b,
        _client_c: client_c,
        project_a,
    }
}

#[gpui::test]
async fn test_auto_watch_toggle_on_off(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let setup = setup_auto_watch_test(&mut server, cx_a, cx_b, cx_c).await;
    let (workspace_a, cx_a) = setup.client_a.build_workspace(&setup.project_a, cx_a);

    workspace_a.update(cx_a, |workspace, _| {
        assert!(!workspace.is_auto_watching_screens());
    });

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    workspace_a.update(cx_a, |workspace, _| {
        assert!(workspace.is_auto_watching_screens());
    });

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    workspace_a.update(cx_a, |workspace, _| {
        assert!(!workspace.is_auto_watching_screens());
    });
}

#[gpui::test]
async fn test_auto_watch_opens_first_available_share(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let setup = setup_auto_watch_test(&mut server, cx_a, cx_b, cx_c).await;
    let (workspace_a, cx_a) = setup.client_a.build_workspace(&setup.project_a, cx_a);
    executor.run_until_parked();

    start_screen_share(cx_b).await;
    executor.run_until_parked();

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        let active_item = workspace.active_item(cx).expect("no active item");
        assert_eq!(
            active_item.tab_content_text(0, cx),
            "user_b's screen",
            "should be viewing user_b's screen"
        );
    });
}

#[gpui::test]
async fn test_auto_watch_opens_share_when_waiting(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let setup = setup_auto_watch_test(&mut server, cx_a, cx_b, cx_c).await;
    let (workspace_a, cx_a) = setup.client_a.build_workspace(&setup.project_a, cx_a);

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    workspace_a.update(cx_a, |workspace, _| {
        assert!(workspace.is_auto_watching_screens());
    });

    start_screen_share(cx_b).await;
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        let active_item = workspace.active_item(cx).expect("no active item");
        assert_eq!(
            active_item.tab_content_text(0, cx),
            "user_b's screen",
            "should be viewing user_b's screen"
        );
    });
}

#[gpui::test]
async fn test_auto_watch_switches_on_share_end(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let setup = setup_auto_watch_test(&mut server, cx_a, cx_b, cx_c).await;
    let (workspace_a, cx_a) = setup.client_a.build_workspace(&setup.project_a, cx_a);

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });

    start_screen_share(cx_b).await;
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_b's screen"
        );
    });

    start_screen_share(cx_c).await;
    executor.run_until_parked();

    stop_screen_share(cx_b);
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_c's screen",
            "should switch to user_c's screen after user_b stops sharing"
        );
    });
}

#[gpui::test]
async fn test_auto_watch_toggle_off_leaves_tabs_open(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let setup = setup_auto_watch_test(&mut server, cx_a, cx_b, cx_c).await;
    let (workspace_a, cx_a) = setup.client_a.build_workspace(&setup.project_a, cx_a);

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    start_screen_share(cx_b).await;
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_b's screen"
        );
    });

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });

    workspace_a.update(cx_a, |workspace, cx| {
        assert!(!workspace.is_auto_watching_screens());
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_b's screen",
            "screen share tab should remain open after toggling off"
        );
    });
}

#[gpui::test]
async fn test_auto_watch_contextual_focus_user_viewing(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let setup = setup_auto_watch_test(&mut server, cx_a, cx_b, cx_c).await;
    let (workspace_a, cx_a) = setup.client_a.build_workspace(&setup.project_a, cx_a);

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    start_screen_share(cx_b).await;
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_b's screen"
        );
    });

    start_screen_share(cx_c).await;
    executor.run_until_parked();

    stop_screen_share(cx_b);
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_c's screen",
            "should switch focus to user_c's screen since user was viewing user_b's"
        );
    });
}

#[gpui::test]
async fn test_auto_watch_contextual_focus_user_navigated_away(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    cx_c: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let client_a = server.create_client(cx_a, "user_a").await;
    let _client_b = server.create_client(cx_b, "user_b").await;
    let _client_c = server.create_client(cx_c, "user_c").await;
    server
        .create_room(&mut [(&client_a, cx_a), (&_client_b, cx_b), (&_client_c, cx_c)])
        .await;

    let active_call_a = cx_a.read(ActiveCall::global);

    client_a
        .fs()
        .insert_tree(path!("/a"), json!({ "file.txt": "hello" }))
        .await;
    let (project_a, worktree_id) = client_a.build_local_project(path!("/a"), cx_a).await;
    active_call_a
        .update(cx_a, |call, cx| call.set_location(Some(&project_a), cx))
        .await
        .unwrap();

    let (workspace_a, cx_a) = client_a.build_workspace(&project_a, cx_a);

    let editor_a = workspace_a
        .update_in(cx_a, |workspace, window, cx| {
            workspace.open_path(
                (worktree_id, util::rel_path::rel_path("file.txt")),
                None,
                true,
                window,
                cx,
            )
        })
        .await
        .unwrap();

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.toggle_auto_watch_screens(window, cx);
    });
    start_screen_share(cx_b).await;
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "user_b's screen"
        );
    });

    workspace_a.update_in(cx_a, |workspace, window, cx| {
        workspace.activate_item(&*editor_a, true, true, window, cx);
    });

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "file.txt",
            "user should be looking at their editor"
        );
    });

    start_screen_share(cx_c).await;
    executor.run_until_parked();

    stop_screen_share(cx_b);
    executor.run_until_parked();

    workspace_a.update(cx_a, |workspace, cx| {
        assert_eq!(
            workspace.active_item(cx).unwrap().tab_content_text(0, cx),
            "file.txt",
            "focus should stay on the editor, not switch to user_c's screen"
        );

        let pane = workspace.active_pane().read(cx);
        let screen_tabs: Vec<_> = pane
            .items_of_type::<SharedScreen>()
            .map(|screen| screen.read(cx).tab_content_text(0, cx))
            .collect();
        assert!(
            screen_tabs.iter().any(|t| t == "user_c's screen"),
            "user_c's SharedScreen should be open in the pane, just not focused. tabs: {:?}",
            screen_tabs
        );
    });
}
