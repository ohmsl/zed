use anyhow::anyhow;
use gpui::{AppContext, SharedString, Task, WeakEntity};
use project::ProjectPath;
use std::collections::VecDeque;
use ui::{App, IntoElement, Label, ParentElement, Styled, v_flex};
use workspace::{
    Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

const MAX_UNDO_OPERATIONS: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub enum ProjectPanelOperation {
    Batch(Vec<ProjectPanelOperation>),
    Create { project_path: ProjectPath },
    Rename { from: ProjectPath, to: ProjectPath },
}

pub struct UndoManager {
    workspace: WeakEntity<Workspace>,
    history: VecDeque<ProjectPanelOperation>,
    /// Keeps track of the cursor position in the undo stack so we can easily
    /// undo by picking the current operation in the stack and decreasing the
    /// cursor, as well as redoing, by picking the next operation in the stack
    /// and increasing the cursor.
    cursor: usize,
    /// Maximum number of operations to keep on the undo history.
    limit: usize,
}

impl UndoManager {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self::new_with_limit(workspace, MAX_UNDO_OPERATIONS)
    }

    pub fn new_with_limit(workspace: WeakEntity<Workspace>, limit: usize) -> Self {
        Self {
            workspace,
            limit,
            cursor: 0,
            history: VecDeque::new(),
        }
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.history.len()
    }

    pub fn undo(&mut self, cx: &mut App) {
        if self.cursor == 0 {
            return;
        }

        // We don't currently care whether the undo operation failed or
        // succeeded, so the cursor can always be updated, as we just assume
        // we'll be attempting to undo the next operation, even if undoing
        // the previous one failed.
        self.cursor -= 1;

        if let Some(operation) = self.history.get(self.cursor) {
            let task = self.undo_operation(operation, cx);
            let workspace = self.workspace.clone();

            cx.spawn(async move |cx| {
                let errors = task.await;
                if !errors.is_empty() {
                    cx.update(|cx| {
                        let messages = errors
                            .iter()
                            .map(|err| SharedString::from(err.to_string()))
                            .collect();

                        Self::show_errors(
                            "Failed to undo Project Panel Operation(s)",
                            workspace,
                            messages,
                            cx,
                        )
                    })
                }
            })
            .detach();
        }
    }

    pub fn redo(&mut self, cx: &mut App) {
        if self.cursor >= self.history.len() {
            return;
        }

        if let Some(operation) = self.history.get(self.cursor) {
            let task = self.redo_operation(operation, cx);
            let workspace = self.workspace.clone();

            cx.spawn(async move |cx| {
                let errors = task.await;
                if !errors.is_empty() {
                    cx.update(|cx| {
                        let messages = errors
                            .iter()
                            .map(|err| SharedString::from(err.to_string()))
                            .collect();

                        Self::show_errors(
                            "Failed to redo Project Panel Operation(s)",
                            workspace,
                            messages,
                            cx,
                        )
                    })
                }
            })
            .detach();
        }

        self.cursor += 1;
    }

    fn redo_operation(
        &self,
        operation: &ProjectPanelOperation,
        cx: &mut App,
    ) -> Task<Vec<anyhow::Error>> {
        match operation {
            ProjectPanelOperation::Rename {
                from: old_path,
                to: new_path,
            } => self.rename(old_path, new_path, cx),
            ProjectPanelOperation::Batch(operations) => {
                // Ensure that, when redoing a batch of operations, we do these
                // in the same order as they were passed to the batch, as there
                // might be dependencies between them.
                //
                // For example, imagine the following batch of operations:
                //
                // 1. Create `src/`
                // 2. Create `src/main.rs`
                //
                // If these are not done in order, there's no guarantee that the
                // second one succeeds, as the command to redo the `src/main.rs`
                // file creation might fail unless it also forces the parent
                // directory to be created first.
                let tasks: Vec<_> = operations
                    .into_iter()
                    .map(|operation| self.redo_operation(operation, cx))
                    .collect();

                Self::run_sequentially(tasks, cx)
            }
            ProjectPanelOperation::Create { .. } => Task::ready(vec![anyhow!(
                "Redoing create operations is currently not supported."
            )]),
        }
    }

    pub fn record(&mut self, operation: ProjectPanelOperation) {
        // Recording a new operation while the cursor is not at the end of the
        // undo history should remove all operations from the cursor position to
        // the end instead of inserting an operation in the middle of the undo
        // history.
        if self.cursor < self.history.len() {
            self.history.drain(self.cursor..);
        }

        // The `cursor` is only increased in the case where the history's length
        // is not yet at the limit, because when it is, the `cursor` value
        // should already match `limit`.
        if self.history.len() >= self.limit {
            self.history.pop_front();
            self.history.push_back(operation);
        } else {
            self.history.push_back(operation);
            self.cursor += 1;
        }
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

    /// Attempts to revert the provided `operation`, returning a vector of errors
    /// in case there was any failure while reverting the operation.
    ///
    /// For all operations other than [`crate::undo::ProjectPanelOperation::Batch`], a maximum
    /// of one error is returned.
    fn undo_operation(
        &self,
        operation: &ProjectPanelOperation,
        cx: &mut App,
    ) -> Task<Vec<anyhow::Error>> {
        match operation {
            ProjectPanelOperation::Create { project_path } => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return Task::ready(vec![anyhow!("Failed to obtain workspace.")]);
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
                    Err(err) => return Task::ready(vec![err]),
                };

                cx.spawn(async move |_| match task.await {
                    Ok(_) => vec![],
                    Err(err) => vec![err],
                })
            }
            ProjectPanelOperation::Rename {
                from: old_path,
                to: new_path,
            } => self.rename(new_path, old_path, cx),
            ProjectPanelOperation::Batch(operations) => {
                // When reverting operations in a batch, we reverse the order of
                // operations to handle dependencies between them. For example,
                // if a batch contains the following order of operations:
                //
                // 1. Create `src/`
                // 2. Create `src/main.rs`
                //
                // If we first try to revert the directory creation, it would
                // fail because there's still files inside the directory.
                // Operations are also reverted sequentially in order to avoid
                // this same problem.
                let tasks: Vec<_> = operations
                    .into_iter()
                    .rev()
                    .map(|operation| self.undo_operation(operation, cx))
                    .collect();

                Self::run_sequentially(tasks, cx)
            }
        }
    }

    fn rename(
        &self,
        from: &ProjectPath,
        to: &ProjectPath,
        cx: &mut App,
    ) -> Task<Vec<anyhow::Error>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(vec![anyhow!("Failed to obtain workspace.")]);
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
            Err(err) => return Task::ready(vec![err]),
        };

        cx.spawn(async move |_| match task.await {
            Ok(_) => vec![],
            Err(err) => vec![err],
        })
    }

    /// Awaits each task in `tasks` sequentially, collecting any errors.
    ///
    /// Sequential execution is important for [`ProjectPanelOperation::Batch`]
    /// operations, where there may be ordering dependencies between tasks. For
    /// example, a directory must be created before files can be placed inside
    /// it.
    fn run_sequentially(
        tasks: Vec<Task<Vec<anyhow::Error>>>,
        cx: &mut App,
    ) -> Task<Vec<anyhow::Error>> {
        cx.spawn(async move |_| {
            let mut errors = Vec::new();

            for task in tasks {
                errors.extend(task.await);
            }

            errors
        })
    }

    /// Displays a notification with the list of provided errors ensuring that,
    /// when more than one error is provided, which can be the case when dealing
    /// with undoing a [`crate::undo::ProjectPanelOperation::Batch`], a list is
    /// displayed with each of the errors, instead of a single message.
    fn show_errors(
        title: impl Into<SharedString>,
        workspace: WeakEntity<Workspace>,
        messages: Vec<SharedString>,
        cx: &mut App,
    ) {
        workspace
            .update(cx, move |workspace, cx| {
                let notification_id =
                    NotificationId::Named(SharedString::new_static("project_panel_undo"));

                workspace.show_notification(notification_id, cx, move |cx| {
                    cx.new(|cx| {
                        if let [err] = messages.as_slice() {
                            MessageNotification::new(err.to_string(), cx).with_title(title)
                        } else {
                            MessageNotification::new_from_builder(cx, move |_, _| {
                                v_flex()
                                    .gap_1()
                                    .children(
                                        messages
                                            .iter()
                                            .map(|message| Label::new(format!("- {message}"))),
                                    )
                                    .into_any_element()
                            })
                            .with_title(title)
                        }
                    })
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
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use project::{FakeFs, Project, ProjectPath, WorktreeId};
    use std::sync::Arc;
    use util::rel_path::rel_path;
    use workspace::MultiWorkspace;

    struct TestContext {
        project: Entity<Project>,
        panel: Entity<ProjectPanel>,
    }

    async fn init_test(cx: &mut TestAppContext) -> TestContext {
        project_panel_tests::init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        let panel = workspace.update_in(cx, ProjectPanel::new);
        cx.run_until_parked();

        TestContext { project, panel }
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

    #[gpui::test]
    async fn test_limit(cx: &mut TestAppContext) {
        let test_context = init_test(cx).await;
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

        test_context.panel.update(cx, move |panel, _cx| {
            panel.undo_manager = UndoManager::new_with_limit(panel.workspace.clone(), 3);
            panel.undo_manager.record(operation_a);
            panel.undo_manager.record(operation_b);
            panel.undo_manager.record(operation_c);
            panel.undo_manager.record(operation_d);

            assert_eq!(panel.undo_manager.history.len(), 3);
        });
    }

    #[gpui::test]
    async fn test_cursor(cx: &mut TestAppContext) {
        let test_context = init_test(cx).await;
        let worktree_id = test_context.project.update(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });

        test_context.panel.update(cx, |panel, _cx| {
            panel.undo_manager = UndoManager::new_with_limit(panel.workspace.clone(), 3);
            panel
                .undo_manager
                .record(build_create_operation(worktree_id, "file_a.txt"));

            assert_eq!(panel.undo_manager.cursor, 1);
        });

        test_context.panel.update(cx, |panel, cx| {
            panel.undo_manager.undo(cx);

            // Ensure that only the `UndoManager::cursor` is updated, as the
            // history should remain unchanged, so we can later redo the
            // operation.
            assert_eq!(panel.undo_manager.cursor, 0);
            assert_eq!(
                panel.undo_manager.history,
                vec![build_create_operation(worktree_id, "file_a.txt")]
            );

            panel.undo_manager.undo(cx);

            // Undoing when cursor is already at `0` should have no effect on
            // both the `cursor` and `history`.
            assert_eq!(panel.undo_manager.cursor, 0);
            assert_eq!(
                panel.undo_manager.history,
                vec![build_create_operation(worktree_id, "file_a.txt")]
            );
        });

        test_context.panel.update(cx, |panel, cx| {
            panel.undo_manager.redo(cx);

            // Ensure that only the `UndoManager::cursor` is updated, since
            // we're only re-doing an operation that was already part of the
            // undo history.
            assert_eq!(panel.undo_manager.cursor, 1);
            assert_eq!(
                panel.undo_manager.history,
                vec![build_create_operation(worktree_id, "file_a.txt")]
            );
        });

        test_context.panel.update(cx, |panel, _cx| {
            panel
                .undo_manager
                .record(build_create_operation(worktree_id, "file_b.txt"));
            panel
                .undo_manager
                .record(build_create_operation(worktree_id, "file_c.txt"));

            assert_eq!(panel.undo_manager.cursor, panel.undo_manager.limit);

            panel
                .undo_manager
                .record(build_create_operation(worktree_id, "file_d.txt"));

            // Ensure that the operation to create `file_a.txt` has been evicted
            // but the cursor has not grown when that new operation was
            // recorded, as the history was already at its limit.
            assert_eq!(panel.undo_manager.cursor, panel.undo_manager.limit);
            assert_eq!(
                panel.undo_manager.history,
                vec![
                    build_create_operation(worktree_id, "file_b.txt"),
                    build_create_operation(worktree_id, "file_c.txt"),
                    build_create_operation(worktree_id, "file_d.txt")
                ]
            );
        });

        // We'll now undo 2 operations, ensuring that the `cursor` is updated
        // accordingly. Afterwards, we'll record a new operation and verify that
        // the `cursor` is incremented but that all operations from the previous
        // cursor position onwards are discarded.
        test_context.panel.update(cx, |panel, cx| {
            panel.undo_manager.undo(cx);
            panel.undo_manager.undo(cx);

            assert_eq!(panel.undo_manager.cursor, 1);
            assert_eq!(
                panel.undo_manager.history,
                vec![
                    build_create_operation(worktree_id, "file_b.txt"),
                    build_create_operation(worktree_id, "file_c.txt"),
                    build_create_operation(worktree_id, "file_d.txt")
                ]
            );

            panel
                .undo_manager
                .record(build_create_operation(worktree_id, "file_e.txt"));

            assert_eq!(panel.undo_manager.cursor, 2);
            assert_eq!(
                panel.undo_manager.history,
                vec![
                    build_create_operation(worktree_id, "file_b.txt"),
                    build_create_operation(worktree_id, "file_e.txt"),
                ]
            );
        });
    }
}
