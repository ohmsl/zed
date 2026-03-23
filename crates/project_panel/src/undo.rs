use crate::ProjectPanel;
use anyhow::{Result, anyhow};
use gpui::{AppContext, SharedString, Task, WeakEntity};
use project::ProjectPath;
use std::collections::VecDeque;
use ui::App;
use workspace::{
    Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

const MAX_UNDO_OPERATIONS: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub enum ProjectPanelOperation {
    Batch(Vec<ProjectPanelOperation>),
    Create { project_path: ProjectPath },
    Trash { project_path: ProjectPath },
    Rename { from: ProjectPath, to: ProjectPath },
}

impl ProjectPanelOperation {
    fn inverse(&self) -> Self {
        match self {
            Self::Create { project_path } => Self::Trash {
                project_path: project_path.clone(),
            },
            Self::Trash { project_path } => Self::Create {
                project_path: project_path.clone(),
            },
            Self::Rename { from, to } => Self::Rename {
                from: to.clone(),
                to: from.clone(),
            },
            // When inverting a batch of operations, we reverse the order of
            // operations to handle dependencies between them. For example, if a
            // batch contains the following order of operations:
            //
            // 1. Create `src/`
            // 2. Create `src/main.rs`
            //
            // If we first tried to revert the directory creation, it would fail
            // because there's still files inside the directory.
            Self::Batch(operations) => Self::Batch(
                operations
                    .iter()
                    .rev()
                    .map(|operation| operation.inverse())
                    .collect(),
            ),
        }
    }
}

pub struct UndoManager {
    workspace: WeakEntity<Workspace>,
    panel: WeakEntity<ProjectPanel>,
    undo_stack: VecDeque<ProjectPanelOperation>,
    redo_stack: Vec<ProjectPanelOperation>,
    /// Maximum number of operations to keep on the undo history.
    limit: usize,
}

impl UndoManager {
    pub fn new(workspace: WeakEntity<Workspace>, panel: WeakEntity<ProjectPanel>) -> Self {
        Self::new_with_limit(workspace, panel, MAX_UNDO_OPERATIONS)
    }

    pub fn new_with_limit(
        workspace: WeakEntity<Workspace>,
        panel: WeakEntity<ProjectPanel>,
        limit: usize,
    ) -> Self {
        Self {
            workspace,
            panel,
            limit,
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self, cx: &mut App) {
        if let Some(operation) = self.undo_stack.pop_back() {
            let task = self.execute_operation(&operation, cx);
            let panel = self.panel.clone();
            let workspace = self.workspace.clone();

            cx.spawn(async move |cx| match task.await {
                Ok(operation) => panel.update(cx, |panel, _cx| {
                    panel.undo_manager.redo_stack.push(operation)
                }),
                Err(err) => cx.update(|cx| {
                    Self::show_error(
                        "Failed to undo Project Panel Operation(s)",
                        workspace,
                        err.to_string().into(),
                        cx,
                    );

                    Ok(())
                }),
            })
            .detach();
        }
    }

    pub fn redo(&mut self, cx: &mut App) {
        if let Some(operation) = self.redo_stack.pop() {
            let task = self.execute_operation(&operation, cx);
            let panel = self.panel.clone();
            let workspace = self.workspace.clone();

            cx.spawn(async move |cx| match task.await {
                Ok(operation) => panel.update(cx, |panel, _cx| {
                    panel.undo_manager.undo_stack.push_back(operation)
                }),
                Err(err) => cx.update(|cx| {
                    Self::show_error(
                        "Failed to redo Project Panel Operation(s)",
                        workspace,
                        err.to_string().into(),
                        cx,
                    );

                    Ok(())
                }),
            })
            .detach();
        }
    }

    pub fn record(&mut self, operation: ProjectPanelOperation) {
        // Recording a new operation while there's still operations in the
        // `redo_stack` should clear all operations from the `redo_stack`, as we
        // might end up in a situation where the state diverges and the
        // `redo_stack` operations can no longer be done.
        if !self.redo_stack.is_empty() {
            self.redo_stack.clear();
        }

        if self.undo_stack.len() >= self.limit {
            self.undo_stack.pop_front();
        }

        self.undo_stack.push_back(operation.inverse());
    }

    pub fn record_batch(&mut self, operations: impl IntoIterator<Item = ProjectPanelOperation>) {
        let mut operations = operations.into_iter().collect::<Vec<_>>();
        let operation = match operations.len() {
            0 => return,
            1 => operations.pop().unwrap(),
            _ => ProjectPanelOperation::Batch(operations),
        };

        self.record(operation);
    }

    /// Attempts to execute the provided operation, returning the inverse of the
    /// provided `operation` as a result.
    fn execute_operation(
        &mut self,
        operation: &ProjectPanelOperation,
        cx: &mut App,
    ) -> Task<Result<ProjectPanelOperation>> {
        match operation {
            ProjectPanelOperation::Rename { from, to } => self.rename(from, to, cx),
            ProjectPanelOperation::Trash { project_path } => self.trash(project_path, cx),
            ProjectPanelOperation::Create { project_path } => self.create(project_path, cx),
            ProjectPanelOperation::Batch(operations) => self.batch(operations, cx),
        }
    }

    fn rename(
        &self,
        from: &ProjectPath,
        to: &ProjectPath,
        cx: &mut App,
    ) -> Task<Result<ProjectPanelOperation>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow!("Failed to obtain workspace.")));
        };

        let result = workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                let entry_id = project
                    .entry_for_path(from, cx)
                    .map(|entry| entry.id)
                    .ok_or_else(|| anyhow!("No entry for path."))?;

                Ok(project.rename_entry(entry_id, to.clone(), cx))
            })
        });

        let task = match result {
            Ok(task) => task,
            Err(err) => return Task::ready(Err(err)),
        };

        let from = from.clone();
        let to = to.clone();
        cx.spawn(async move |_| match task.await {
            Err(err) => Err(err),
            Ok(_) => Ok(ProjectPanelOperation::Rename {
                from: to.clone(),
                to: from.clone(),
            }),
        })
    }

    fn create(
        &self,
        project_path: &ProjectPath,
        cx: &mut App,
    ) -> Task<Result<ProjectPanelOperation>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow!("Failed to obtain workspace.")));
        };

        let task = workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                // This should not be hardcoded to `false`, as it can genuinely
                // be a directory and it misses all the nuances and details from
                // `ProjectPanel::confirm_edit`. However, we expect this to be a
                // short-lived solution as we add support for restoring trashed
                // files, at which point we'll no longer need to `Create` new
                // files, any redoing of a trash operation should be a restore.
                let is_directory = false;
                project.create_entry(project_path.clone(), is_directory, cx)
            })
        });

        let project_path = project_path.clone();
        cx.spawn(async move |_| match task.await {
            Ok(_) => Ok(ProjectPanelOperation::Trash { project_path }),
            Err(err) => Err(err),
        })
    }

    fn trash(
        &self,
        project_path: &ProjectPath,
        cx: &mut App,
    ) -> Task<Result<ProjectPanelOperation>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow!("Failed to obtain workspace.")));
        };

        let result = workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                let entry_id = project
                    .entry_for_path(&project_path, cx)
                    .map(|entry| entry.id)
                    .ok_or_else(|| anyhow!("No entry for path."))?;

                project
                    .delete_entry(entry_id, true, cx)
                    .ok_or_else(|| anyhow!("Failed to trash entry."))
            })
        });

        let task = match result {
            Ok(task) => task,
            Err(err) => return Task::ready(Err(err)),
        };

        let project_path = project_path.clone();
        cx.spawn(async move |_| match task.await {
            // We'll want this to eventually be a `Restore` operation, once
            // we've added support, in `fs` to track and restore a trashed file.
            Ok(_) => Ok(ProjectPanelOperation::Create { project_path }),
            Err(err) => Err(err),
        })
    }

    fn batch(
        &mut self,
        operations: &[ProjectPanelOperation],
        cx: &mut App,
    ) -> Task<Result<ProjectPanelOperation>> {
        let tasks: Vec<_> = operations
            .into_iter()
            .map(|operation| self.execute_operation(operation, cx))
            .collect();

        cx.spawn(async move |_| {
            let mut operations = Vec::new();

            for task in tasks {
                match task.await {
                    Ok(operation) => operations.push(operation),
                    Err(err) => return Err(err),
                }
            }

            // Return the `ProjectPanelOperation::Batch` that reverses all of
            // the provided operations. The order of operations should be reversed
            // so that dependencies are handled correctly.
            operations.reverse();
            Ok(ProjectPanelOperation::Batch(operations))
        })
    }

    /// Displays a notification with the provided `title` and `error`.
    fn show_error(
        title: impl Into<SharedString>,
        workspace: WeakEntity<Workspace>,
        error: SharedString,
        cx: &mut App,
    ) {
        workspace
            .update(cx, move |workspace, cx| {
                let notification_id =
                    NotificationId::Named(SharedString::new_static("project_panel_undo"));

                workspace.show_notification(notification_id, cx, move |cx| {
                    cx.new(|cx| MessageNotification::new(error.to_string(), cx).with_title(title))
                })
            })
            .ok();
    }
}

#[cfg(test)]
pub(crate) mod test {
    use crate::{
        ProjectPanel, project_panel_tests,
        undo::{ProjectPanelOperation, UndoManager},
    };
    use gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};
    use project::{FakeFs, Project, ProjectPath, WorktreeId};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use util::rel_path::rel_path;
    use workspace::MultiWorkspace;

    struct TestContext {
        project: Entity<Project>,
        panel: Entity<ProjectPanel>,
        window: WindowHandle<MultiWorkspace>,
    }

    async fn init_test(cx: &mut TestAppContext, tree: Option<Value>) -> TestContext {
        project_panel_tests::init_test(cx);

        let fs = FakeFs::new(cx.executor());
        if let Some(tree) = tree {
            fs.insert_tree("/root", tree).await;
        }
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        let panel = workspace.update_in(cx, ProjectPanel::new);
        cx.run_until_parked();

        TestContext {
            project,
            panel,
            window,
        }
    }

    pub(crate) fn build_create_operation(
        worktree_id: WorktreeId,
        file_name: &str,
    ) -> ProjectPanelOperation {
        ProjectPanelOperation::Create {
            project_path: ProjectPath {
                path: Arc::from(rel_path(file_name)),
                worktree_id,
            },
        }
    }

    pub(crate) fn build_trash_operation(
        worktree_id: WorktreeId,
        file_name: &str,
    ) -> ProjectPanelOperation {
        ProjectPanelOperation::Trash {
            project_path: ProjectPath {
                path: Arc::from(rel_path(file_name)),
                worktree_id,
            },
        }
    }

    pub(crate) fn build_rename_operation(
        worktree_id: WorktreeId,
        from: &str,
        to: &str,
    ) -> ProjectPanelOperation {
        let from_path = Arc::from(rel_path(from));
        let to_path = Arc::from(rel_path(to));

        ProjectPanelOperation::Rename {
            from: ProjectPath {
                worktree_id,
                path: from_path,
            },
            to: ProjectPath {
                worktree_id,
                path: to_path,
            },
        }
    }

    async fn rename(
        panel: &Entity<ProjectPanel>,
        from: &str,
        to: &str,
        cx: &mut VisualTestContext,
    ) {
        project_panel_tests::select_path(panel, from, cx);
        panel.update_in(cx, |panel, window, cx| {
            panel.rename(&Default::default(), window, cx)
        });
        cx.run_until_parked();

        panel
            .update_in(cx, |panel, window, cx| {
                panel
                    .filename_editor
                    .update(cx, |editor, cx| editor.set_text(to, window, cx));
                panel.confirm_edit(true, window, cx).unwrap()
            })
            .await
            .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_limit(cx: &mut TestAppContext) {
        let test_context = init_test(cx, None).await;
        let worktree_id = test_context.project.update(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });

        // Since we're updating the `ProjectPanel`'s undo manager with one whose
        // limit is 3 operations, we only need to create 4 operations which
        // we'll record, in order to confirm that the oldest operation is
        // evicted.
        let operation_a = build_create_operation(worktree_id, "file_a.txt");
        let operation_b = build_create_operation(worktree_id, "file_b.txt");
        let operation_c = build_create_operation(worktree_id, "file_c.txt");
        let operation_d = build_create_operation(worktree_id, "file_d.txt");

        test_context.panel.update(cx, move |panel, cx| {
            panel.undo_manager =
                UndoManager::new_with_limit(panel.workspace.clone(), cx.weak_entity(), 3);
            panel.undo_manager.record(operation_a);
            panel.undo_manager.record(operation_b);
            panel.undo_manager.record(operation_c);
            panel.undo_manager.record(operation_d);

            assert_eq!(panel.undo_manager.undo_stack.len(), 3);
        });
    }
    #[gpui::test]
    async fn test_undo_redo_stacks(cx: &mut TestAppContext) {
        let TestContext {
            window,
            panel,
            project,
            ..
        } = init_test(
            cx,
            Some(json!({
                "a.txt": "",
                "b.txt": ""
            })),
        )
        .await;
        let worktree_id = project.update(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });
        let cx = &mut VisualTestContext::from_window(window.into(), cx);

        // Start by renaming `src/file_a.txt` to `src/file_1.txt` and asserting
        // we get the correct inverse operation in the
        // `UndoManager::undo_stackand asserting we get the correct inverse
        // operation in the `UndoManager::undo_stack`.
        rename(&panel, "root/a.txt", "1.txt", cx).await;
        panel.update(cx, |panel, _cx| {
            assert_eq!(
                panel.undo_manager.undo_stack,
                vec![build_rename_operation(worktree_id, "1.txt", "a.txt")]
            );
            assert!(panel.undo_manager.redo_stack.is_empty());
        });

        // After undoing, the operation to be executed should be popped from
        // `UndoManager::undo_stack` and its inverse operation pushed to
        // `UndoManager::redo_stack`.
        panel.update_in(cx, |panel, window, cx| {
            panel.undo(&Default::default(), window, cx);
        });
        cx.run_until_parked();

        panel.update(cx, |panel, _cx| {
            assert!(panel.undo_manager.undo_stack.is_empty());
            assert_eq!(
                panel.undo_manager.redo_stack,
                vec![build_rename_operation(worktree_id, "a.txt", "1.txt")]
            );
        });

        // Redoing should have the same effect as undoing, but in reverse.
        panel.update_in(cx, |panel, window, cx| {
            panel.redo(&Default::default(), window, cx);
        });
        cx.run_until_parked();

        panel.update(cx, |panel, _cx| {
            assert_eq!(
                panel.undo_manager.undo_stack,
                vec![build_rename_operation(worktree_id, "1.txt", "a.txt")]
            );
            assert!(panel.undo_manager.redo_stack.is_empty());
        });
    }

    #[gpui::test]
    async fn test_undo_redo_trash(cx: &mut TestAppContext) {
        let TestContext {
            window,
            panel,
            project,
            ..
        } = init_test(
            cx,
            Some(json!({
                "a.txt": "",
                "b.txt": ""
            })),
        )
        .await;
        let worktree_id = project.update(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });
        let cx = &mut VisualTestContext::from_window(window.into(), cx);

        // Start by setting up the `UndoManager::undo_stack` such that, undoing
        // the last user operation will trash `a.txt`.
        panel.update(cx, |panel, _cx| {
            panel
                .undo_manager
                .undo_stack
                .push_back(build_trash_operation(worktree_id, "a.txt"));
        });

        // Undoing should now delete the file and update the
        // `UndoManager::redo_stack` state with a new `Create` operation.
        panel.update_in(cx, |panel, window, cx| {
            panel.undo(&Default::default(), window, cx);
        });
        cx.run_until_parked();

        panel.update(cx, |panel, _cx| {
            assert!(panel.undo_manager.undo_stack.is_empty());
            assert_eq!(
                panel.undo_manager.redo_stack,
                vec![build_create_operation(worktree_id, "a.txt")]
            );
        });

        // Redoing should create the file again and pop the operation from
        // `UndoManager::redo_stack`.
        panel.update_in(cx, |panel, window, cx| {
            panel.redo(&Default::default(), window, cx);
        });
        cx.run_until_parked();

        panel.update(cx, |panel, _cx| {
            assert_eq!(
                panel.undo_manager.undo_stack,
                vec![build_trash_operation(worktree_id, "a.txt")]
            );
            assert!(panel.undo_manager.redo_stack.is_empty());
        });
    }

    #[gpui::test]
    async fn test_undo_redo_batch(cx: &mut TestAppContext) {
        let TestContext {
            window,
            panel,
            project,
            ..
        } = init_test(
            cx,
            Some(json!({
                "a.txt": "",
                "b.txt": ""
            })),
        )
        .await;
        let worktree_id = project.update(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });
        let cx = &mut VisualTestContext::from_window(window.into(), cx);

        // There's currently no way to trigger two file renames in a single
        // operation using the `ProjectPanel`. As such, we'll directly record
        // the batch of operations in `UndoManager`, simulating that `1.txt` and
        // `2.txt` had been renamed to `a.txt` and `b.txt`, respectively.
        panel.update(cx, |panel, _cx| {
            panel.undo_manager.record_batch(vec![
                build_rename_operation(worktree_id, "1.txt", "a.txt"),
                build_rename_operation(worktree_id, "2.txt", "b.txt"),
            ]);

            assert_eq!(
                panel.undo_manager.undo_stack,
                vec![ProjectPanelOperation::Batch(vec![
                    build_rename_operation(worktree_id, "b.txt", "2.txt"),
                    build_rename_operation(worktree_id, "a.txt", "1.txt"),
                ])]
            );
            assert!(panel.undo_manager.redo_stack.is_empty());
        });

        panel.update_in(cx, |panel, window, cx| {
            panel.undo(&Default::default(), window, cx);
        });
        cx.run_until_parked();

        // Since the operations in the `Batch` are meant to be done in order,
        // the inverse should have the operations in the opposite order to avoid
        // dependencies. For example, creating a `src/` folder come before
        // creating the `src/file_a.txt` file, but when undoing, the file should
        // be trashed first.
        panel.update(cx, |panel, _cx| {
            assert!(panel.undo_manager.undo_stack.is_empty());
            assert_eq!(
                panel.undo_manager.redo_stack,
                vec![ProjectPanelOperation::Batch(vec![
                    build_rename_operation(worktree_id, "1.txt", "a.txt"),
                    build_rename_operation(worktree_id, "2.txt", "b.txt"),
                ])]
            );
        });

        panel.update_in(cx, |panel, window, cx| {
            panel.redo(&Default::default(), window, cx);
        });
        cx.run_until_parked();

        panel.update(cx, |panel, _cx| {
            assert_eq!(
                panel.undo_manager.undo_stack,
                vec![ProjectPanelOperation::Batch(vec![
                    build_rename_operation(worktree_id, "b.txt", "2.txt"),
                    build_rename_operation(worktree_id, "a.txt", "1.txt"),
                ])]
            );
            assert!(panel.undo_manager.redo_stack.is_empty());
        });
    }
}
