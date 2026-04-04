use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_client_protocol as acp;
use anyhow::{Context as _, Result, anyhow};
use gpui::{App, AsyncApp, Entity, Global, Task, WindowHandle};
use parking_lot::Mutex;
use project::{LocalProjectFlags, Project, WorktreeId, git_store::Repository};
use util::ResultExt;
use workspace::{
    AppState, MultiWorkspace, OpenMode, OpenOptions, PathList, Toast, Workspace,
    notifications::NotificationId, open_new, open_paths,
};

use crate::thread_metadata_store::ThreadMetadataStore;

#[derive(Default)]
pub struct ThreadArchiveCleanupCoordinator {
    in_flight_roots: Mutex<HashSet<PathBuf>>,
}

impl Global for ThreadArchiveCleanupCoordinator {}

fn ensure_global(cx: &mut App) {
    if !cx.has_global::<ThreadArchiveCleanupCoordinator>() {
        cx.set_global(ThreadArchiveCleanupCoordinator::default());
    }
}

#[derive(Clone)]
pub struct ArchiveOutcome {
    pub archived_immediately: bool,
    pub roots_to_delete: Vec<PathBuf>,
}

#[derive(Clone)]
struct RootPlan {
    root_path: PathBuf,
    main_repo_path: PathBuf,
    affected_projects: Vec<AffectedProject>,
}

#[derive(Clone)]
struct AffectedProject {
    project: Entity<Project>,
    worktree_id: WorktreeId,
}

#[derive(Clone)]
enum FallbackTarget {
    ExistingWorkspace {
        window: WindowHandle<MultiWorkspace>,
        workspace: Entity<Workspace>,
    },
    OpenPaths {
        requesting_window: WindowHandle<MultiWorkspace>,
        paths: Vec<PathBuf>,
    },
    OpenEmpty {
        requesting_window: WindowHandle<MultiWorkspace>,
    },
}

#[derive(Clone)]
struct CleanupPlan {
    roots: Vec<RootPlan>,
    current_workspace: Option<Entity<Workspace>>,
    current_workspace_will_be_empty: bool,
    fallback: Option<FallbackTarget>,
    affected_workspaces: Vec<Entity<Workspace>>,
}

pub fn archive_thread(
    session_id: &acp::SessionId,
    current_workspace: Option<Entity<Workspace>>,
    window: WindowHandle<MultiWorkspace>,
    cx: &mut App,
) -> ArchiveOutcome {
    ensure_global(cx);
    let plan = build_cleanup_plan(session_id, current_workspace, window, cx);

    ThreadMetadataStore::global(cx).update(cx, |store, cx| store.archive(session_id, cx));

    if let Some(plan) = plan {
        let roots_to_delete = plan
            .roots
            .iter()
            .map(|root| root.root_path.clone())
            .collect::<Vec<_>>();
        if !roots_to_delete.is_empty() {
            cx.spawn(async move |cx| {
                run_cleanup(plan, cx).await;
            })
            .detach();

            return ArchiveOutcome {
                archived_immediately: true,
                roots_to_delete,
            };
        }
    }

    ArchiveOutcome {
        archived_immediately: true,
        roots_to_delete: Vec::new(),
    }
}

fn build_cleanup_plan(
    session_id: &acp::SessionId,
    current_workspace: Option<Entity<Workspace>>,
    requesting_window: WindowHandle<MultiWorkspace>,
    cx: &App,
) -> Option<CleanupPlan> {
    let metadata = ThreadMetadataStore::global(cx)
        .read(cx)
        .entry(session_id)
        .cloned()?;

    let workspaces = all_open_workspaces(cx);

    let candidate_roots = metadata
        .folder_paths
        .ordered_paths()
        .filter_map(|path| build_root_plan(path, &workspaces, cx))
        .filter(|plan| {
            !path_is_referenced_by_other_unarchived_threads(session_id, &plan.root_path, cx)
        })
        .collect::<Vec<_>>();

    if candidate_roots.is_empty() {
        return Some(CleanupPlan {
            roots: Vec::new(),
            current_workspace,
            current_workspace_will_be_empty: false,
            fallback: None,
            affected_workspaces: Vec::new(),
        });
    }

    let mut affected_workspaces = Vec::new();
    let mut current_workspace_will_be_empty = false;

    for workspace in workspaces.iter() {
        let doomed_root_count = workspace
            .read(cx)
            .root_paths(cx)
            .into_iter()
            .filter(|path| {
                candidate_roots
                    .iter()
                    .any(|root| root.root_path.as_path() == path.as_ref())
            })
            .count();

        if doomed_root_count == 0 {
            continue;
        }

        let surviving_root_count = workspace
            .read(cx)
            .root_paths(cx)
            .len()
            .saturating_sub(doomed_root_count);
        if current_workspace
            .as_ref()
            .is_some_and(|current| current == workspace)
        {
            current_workspace_will_be_empty = surviving_root_count == 0;
        }
        affected_workspaces.push(workspace.clone());
    }

    let fallback = if current_workspace_will_be_empty {
        choose_fallback_target(
            session_id,
            current_workspace.as_ref(),
            &candidate_roots,
            &requesting_window,
            &workspaces,
            cx,
        )
    } else {
        None
    };

    Some(CleanupPlan {
        roots: candidate_roots,
        current_workspace,
        current_workspace_will_be_empty,
        fallback,
        affected_workspaces,
    })
}

fn build_root_plan(path: &Path, workspaces: &[Entity<Workspace>], cx: &App) -> Option<RootPlan> {
    let path = path.to_path_buf();
    let affected_projects = workspaces
        .iter()
        .filter_map(|workspace| {
            let project = workspace.read(cx).project().clone();
            let worktree = project
                .read(cx)
                .visible_worktrees(cx)
                .find(|worktree| worktree.read(cx).abs_path().as_ref() == path.as_path())?;
            let worktree_id = worktree.read(cx).id();
            Some(AffectedProject {
                project,
                worktree_id,
            })
        })
        .collect::<Vec<_>>();

    let linked_snapshot = workspaces
        .iter()
        .flat_map(|workspace| {
            workspace
                .read(cx)
                .project()
                .read(cx)
                .repositories(cx)
                .values()
                .cloned()
                .collect::<Vec<_>>()
        })
        .find_map(|repo| {
            let snapshot = repo.read(cx).snapshot();
            (snapshot.is_linked_worktree()
                && snapshot.work_directory_abs_path.as_ref() == path.as_path())
            .then_some(snapshot)
        })?;

    Some(RootPlan {
        root_path: path,
        main_repo_path: linked_snapshot.original_repo_abs_path.to_path_buf(),
        affected_projects,
    })
}

fn path_is_referenced_by_other_unarchived_threads(
    current_session_id: &acp::SessionId,
    path: &Path,
    cx: &App,
) -> bool {
    ThreadMetadataStore::global(cx)
        .read(cx)
        .entries()
        .filter(|thread| thread.session_id != *current_session_id)
        .filter(|thread| !thread.archived)
        .any(|thread| {
            thread
                .folder_paths
                .paths()
                .iter()
                .any(|other_path| other_path.as_path() == path)
        })
}

fn choose_fallback_target(
    current_session_id: &acp::SessionId,
    current_workspace: Option<&Entity<Workspace>>,
    roots: &[RootPlan],
    requesting_window: &WindowHandle<MultiWorkspace>,
    workspaces: &[Entity<Workspace>],
    cx: &App,
) -> Option<FallbackTarget> {
    let doomed_roots = roots
        .iter()
        .map(|root| root.root_path.clone())
        .collect::<HashSet<_>>();

    let surviving_same_window = requesting_window.read(cx).ok().and_then(|multi_workspace| {
        multi_workspace
            .workspaces()
            .iter()
            .filter(|workspace| current_workspace.is_none_or(|current| *workspace != current))
            .find(|workspace| workspace_survives(workspace, &doomed_roots, cx))
            .cloned()
    });
    if let Some(workspace) = surviving_same_window {
        return Some(FallbackTarget::ExistingWorkspace {
            window: *requesting_window,
            workspace,
        });
    }

    for window in cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
    {
        if window == *requesting_window {
            continue;
        }
        if let Ok(multi_workspace) = window.read(cx) {
            if let Some(workspace) = multi_workspace
                .workspaces()
                .iter()
                .find(|workspace| workspace_survives(workspace, &doomed_roots, cx))
                .cloned()
            {
                return Some(FallbackTarget::ExistingWorkspace { window, workspace });
            }
        }
    }

    let safe_thread_workspace = ThreadMetadataStore::global(cx)
        .read(cx)
        .entries()
        .filter(|metadata| metadata.session_id != *current_session_id && !metadata.archived)
        .filter_map(|metadata| {
            workspaces
                .iter()
                .find(|workspace| workspace_path_list(workspace, cx) == metadata.folder_paths)
                .cloned()
        })
        .find(|workspace| workspace_survives(workspace, &doomed_roots, cx));

    if let Some(workspace) = safe_thread_workspace {
        let window = window_for_workspace(&workspace, cx).unwrap_or(*requesting_window);
        return Some(FallbackTarget::ExistingWorkspace { window, workspace });
    }

    if let Some(root) = roots.first() {
        return Some(FallbackTarget::OpenPaths {
            requesting_window: *requesting_window,
            paths: vec![root.main_repo_path.clone()],
        });
    }

    Some(FallbackTarget::OpenEmpty {
        requesting_window: *requesting_window,
    })
}

async fn run_cleanup(plan: CleanupPlan, cx: &mut AsyncApp) {
    let roots_to_delete =
        cx.update_global::<ThreadArchiveCleanupCoordinator, _>(|coordinator, _cx| {
            let mut in_flight_roots = coordinator.in_flight_roots.lock();
            plan.roots
                .iter()
                .filter_map(|root| {
                    if in_flight_roots.insert(root.root_path.clone()) {
                        Some(root.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        });

    if roots_to_delete.is_empty() {
        return;
    }

    let active_workspace = plan.current_workspace.clone();
    if let Some(workspace) = active_workspace
        .as_ref()
        .filter(|_| plan.current_workspace_will_be_empty)
    {
        let Some(window) = window_for_workspace_async(workspace, cx) else {
            release_in_flight_roots(&roots_to_delete, cx);
            return;
        };

        let should_continue = save_workspace_for_root_removal(workspace.clone(), window, cx).await;
        if !should_continue {
            release_in_flight_roots(&roots_to_delete, cx);
            return;
        }
    }

    for workspace in plan
        .affected_workspaces
        .iter()
        .filter(|workspace| Some((*workspace).clone()) != active_workspace)
    {
        let Some(window) = window_for_workspace_async(workspace, cx) else {
            continue;
        };

        if !save_workspace_for_root_removal(workspace.clone(), window, cx).await {
            release_in_flight_roots(&roots_to_delete, cx);
            return;
        }
    }

    if plan.current_workspace_will_be_empty {
        if let Some(fallback) = plan.fallback.clone() {
            activate_fallback(fallback, cx).await.log_err();
        }
    }

    let mut git_removal_errors: Vec<(PathBuf, anyhow::Error)> = Vec::new();

    for root in &roots_to_delete {
        if let Err(error) = remove_root(root.clone(), cx).await {
            git_removal_errors.push((root.root_path.clone(), error));
        }
    }

    cleanup_empty_workspaces(&plan.affected_workspaces, cx).await;

    if !git_removal_errors.is_empty() {
        let detail = git_removal_errors
            .into_iter()
            .map(|(path, error)| format!("{}: {error}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        show_error_toast(
            "Thread archived, but linked worktree cleanup failed",
            &detail,
            &plan,
            cx,
        );
    }

    release_in_flight_roots(&roots_to_delete, cx);
}

async fn save_workspace_for_root_removal(
    workspace: Entity<Workspace>,
    window: WindowHandle<MultiWorkspace>,
    cx: &mut AsyncApp,
) -> bool {
    let has_dirty_items = workspace.read_with(cx, |workspace, cx| {
        workspace.items(cx).any(|item| item.is_dirty(cx))
    });

    if has_dirty_items {
        let _ = window.update(cx, |multi_workspace, window, cx| {
            window.activate_window();
            multi_workspace.activate(workspace.clone(), window, cx);
        });
    }

    let save_task = window.update(cx, |_multi_workspace, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.save_for_root_removal(window, cx)
        })
    });

    let Ok(task) = save_task else {
        return false;
    };

    task.await.unwrap_or(false)
}

async fn activate_fallback(target: FallbackTarget, cx: &mut AsyncApp) -> Result<()> {
    match target {
        FallbackTarget::ExistingWorkspace { window, workspace } => {
            window.update(cx, |multi_workspace, window, cx| {
                window.activate_window();
                multi_workspace.activate(workspace, window, cx);
            })?;
        }
        FallbackTarget::OpenPaths {
            requesting_window,
            paths,
        } => {
            let app_state = current_app_state(cx).context("no workspace app state available")?;
            cx.update(|cx| {
                open_paths(
                    &paths,
                    app_state,
                    OpenOptions {
                        requesting_window: Some(requesting_window),
                        open_mode: OpenMode::Activate,
                        ..Default::default()
                    },
                    cx,
                )
            })
            .await?;
        }
        FallbackTarget::OpenEmpty { requesting_window } => {
            let app_state = current_app_state(cx).context("no workspace app state available")?;
            cx.update(|cx| {
                open_new(
                    OpenOptions {
                        requesting_window: Some(requesting_window),
                        open_mode: OpenMode::Activate,
                        ..Default::default()
                    },
                    app_state,
                    cx,
                    |_workspace, _window, _cx| {},
                )
            })
            .await?;
        }
    }

    Ok(())
}

async fn remove_root(root: RootPlan, cx: &mut AsyncApp) -> Result<()> {
    let release_tasks: Vec<_> = root
        .affected_projects
        .iter()
        .map(|affected| {
            let project = affected.project.clone();
            let worktree_id = affected.worktree_id;
            project.update(cx, |project, cx| {
                let wait = project.wait_for_worktree_release(worktree_id, cx);
                project.remove_worktree(worktree_id, cx);
                wait
            })
        })
        .collect();

    if let Err(error) = remove_root_after_worktree_removal(&root, release_tasks, cx).await {
        rollback_root(&root, cx).await;
        return Err(error);
    }

    Ok(())
}

async fn remove_root_after_worktree_removal(
    root: &RootPlan,
    release_tasks: Vec<Task<Result<()>>>,
    cx: &mut AsyncApp,
) -> Result<()> {
    for task in release_tasks {
        task.await?;
    }

    let (repo, _temp_project) = repository_for_root_removal(root, cx).await?;
    let receiver = repo.update(cx, |repo: &mut Repository, _cx| {
        repo.remove_worktree(root.root_path.clone(), false)
    });
    let result = receiver
        .await
        .map_err(|_| anyhow!("git worktree removal was canceled"))?;
    result
}

async fn repository_for_root_removal(
    root: &RootPlan,
    cx: &mut AsyncApp,
) -> Result<(Entity<Repository>, Option<Entity<Project>>)> {
    let live_repo = cx.update(|cx| {
        all_open_workspaces(cx)
            .into_iter()
            .flat_map(|workspace| {
                workspace
                    .read(cx)
                    .project()
                    .read(cx)
                    .repositories(cx)
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .find(|repo| {
                repo.read(cx).snapshot().work_directory_abs_path.as_ref()
                    == root.main_repo_path.as_path()
            })
    });

    if let Some(repo) = live_repo {
        return Ok((repo, None));
    }

    let app_state =
        current_app_state(cx).context("no app state available for temporary project")?;
    let temp_project = cx.update(|cx| {
        Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            LocalProjectFlags::default(),
            cx,
        )
    });

    let create_worktree = temp_project.update(cx, |project, cx| {
        project.create_worktree(root.main_repo_path.clone(), true, cx)
    });
    let _worktree = create_worktree.await?;
    let initial_scan = temp_project.read_with(cx, |project, cx| project.wait_for_initial_scan(cx));
    initial_scan.await;

    let repo = temp_project
        .update(cx, |project, cx| {
            project
                .repositories(cx)
                .values()
                .find(|repo| {
                    repo.read(cx).snapshot().work_directory_abs_path.as_ref()
                        == root.main_repo_path.as_path()
                })
                .cloned()
        })
        .context("failed to resolve temporary main repository handle")?;

    let barrier = repo.update(cx, |repo: &mut Repository, _cx| repo.barrier());
    barrier
        .await
        .map_err(|_| anyhow!("temporary repository barrier canceled"))?;
    Ok((repo, Some(temp_project)))
}

async fn rollback_root(root: &RootPlan, cx: &mut AsyncApp) {
    for affected in &root.affected_projects {
        let task = affected.project.update(cx, |project, cx| {
            project.create_worktree(root.root_path.clone(), true, cx)
        });
        let _ = task.await;
    }
}

async fn cleanup_empty_workspaces(workspaces: &[Entity<Workspace>], cx: &mut AsyncApp) {
    for workspace in workspaces {
        let is_empty = workspace.read_with(cx, |workspace, cx| workspace.root_paths(cx).is_empty());
        if !is_empty {
            continue;
        }

        let Some(window) = window_for_workspace_async(workspace, cx) else {
            continue;
        };

        let _ = window.update(cx, |multi_workspace, window, cx| {
            if !multi_workspace.remove(workspace, window, cx) {
                window.remove_window();
            }
        });
    }
}

fn show_error_toast(summary: &str, detail: &str, plan: &CleanupPlan, cx: &mut AsyncApp) {
    let target_workspace = plan
        .current_workspace
        .clone()
        .or_else(|| plan.affected_workspaces.first().cloned());
    let Some(workspace) = target_workspace else {
        return;
    };

    let _ = workspace.update(cx, |workspace, cx| {
        struct ArchiveCleanupErrorToast;
        let message = if detail.is_empty() {
            summary.to_string()
        } else {
            format!("{summary}: {detail}")
        };
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<ArchiveCleanupErrorToast>(),
                message,
            )
            .autohide(),
            cx,
        );
    });
}

fn all_open_workspaces(cx: &App) -> Vec<Entity<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .flat_map(|multi_workspace| {
            multi_workspace
                .read(cx)
                .map(|multi_workspace| multi_workspace.workspaces().to_vec())
                .unwrap_or_default()
        })
        .collect()
}

fn workspace_survives(
    workspace: &Entity<Workspace>,
    doomed_roots: &HashSet<PathBuf>,
    cx: &App,
) -> bool {
    workspace
        .read(cx)
        .root_paths(cx)
        .into_iter()
        .any(|root| !doomed_roots.contains(root.as_ref()))
}

fn workspace_path_list(workspace: &Entity<Workspace>, cx: &App) -> PathList {
    PathList::new(&workspace.read(cx).root_paths(cx))
}

fn window_for_workspace(
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Option<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .find(|window| {
            window
                .read(cx)
                .map(|multi_workspace| multi_workspace.workspaces().contains(workspace))
                .unwrap_or(false)
        })
}

fn window_for_workspace_async(
    workspace: &Entity<Workspace>,
    cx: &mut AsyncApp,
) -> Option<WindowHandle<MultiWorkspace>> {
    let workspace = workspace.clone();
    cx.update(|cx| window_for_workspace(&workspace, cx))
}

fn current_app_state(cx: &mut AsyncApp) -> Option<Arc<AppState>> {
    cx.update(|cx| {
        all_open_workspaces(cx)
            .into_iter()
            .next()
            .map(|workspace| workspace.read(cx).app_state().clone())
    })
}

fn release_in_flight_roots(roots: &[RootPlan], cx: &mut AsyncApp) {
    cx.update_global::<ThreadArchiveCleanupCoordinator, _>(|coordinator, _cx| {
        let mut in_flight_roots = coordinator.in_flight_roots.lock();
        for root in roots {
            in_flight_roots.remove(&root.root_path);
        }
    });
}
