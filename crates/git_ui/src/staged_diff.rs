use crate::{git_panel::GitStatusEntry, resolve_active_repository};
use anyhow::Result;
use buffer_diff::BufferDiff;
use collections::{HashMap, HashSet};
use editor::{
    Editor, EditorEvent, SelectionEffects, multibuffer_context_lines, scroll::Autoscroll,
};
use gpui::{
    App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Render,
    Subscription, Task, WeakEntity,
};
use language::{Anchor, Buffer, Capability, OffsetRangeExt};
use multi_buffer::{MultiBuffer, PathKey};
use project::{
    Project,
    git_store::branch_diff::{self, BranchDiffEvent, DiffBase},
};
use std::any::{Any, TypeId};
use std::sync::Arc;
use ui::prelude::*;
use util::{ResultExt as _, rel_path::RelPath};
use workspace::{
    ItemNavHistory, SerializableItem, Workspace,
    item::{Item, ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};
use ztracing::instrument;

pub struct StagedDiff {
    project: Entity<Project>,
    multibuffer: Entity<MultiBuffer>,
    branch_diff: Entity<branch_diff::BranchDiff>,
    editor: Entity<Editor>,
    buffer_diff_subscriptions: HashMap<Arc<RelPath>, (Entity<BufferDiff>, Subscription)>,
    focus_handle: FocusHandle,
    pending_scroll: Option<PathKey>,
    _task: Task<Result<()>>,
    _subscription: Subscription,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefreshReason {
    DiffChanged,
    StatusesChanged,
}

impl StagedDiff {
    pub(crate) fn register(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
        let _ = workspace;
        workspace::register_serializable_item::<Self>(cx);
    }

    pub fn deploy_at(
        workspace: &mut Workspace,
        entry: Option<GitStatusEntry>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let intended_repo = resolve_active_repository(workspace, cx);
        let existing = workspace.items_of_type::<Self>(cx).next();

        let staged_diff = if let Some(existing) = existing {
            workspace.activate_item(&existing, true, true, window, cx);
            existing
        } else {
            let staged_diff = cx.new(|cx| Self::new(workspace.project().clone(), window, cx));
            workspace.add_item_to_active_pane(
                Box::new(staged_diff.clone()),
                None,
                true,
                window,
                cx,
            );
            staged_diff
        };

        if let Some(intended) = &intended_repo {
            let needs_switch = staged_diff
                .read(cx)
                .branch_diff
                .read(cx)
                .repo()
                .map_or(true, |current| current.read(cx).id != intended.read(cx).id);
            if needs_switch {
                staged_diff.update(cx, |staged_diff, cx| {
                    staged_diff.branch_diff.update(cx, |branch_diff, cx| {
                        branch_diff.set_repo(Some(intended.clone()), cx);
                    });
                });
            }
        }

        if let Some(entry) = entry {
            staged_diff.update(cx, |staged_diff, cx| {
                staged_diff.move_to_entry(entry, window, cx);
            });
        }
    }

    pub(crate) fn new(
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch_diff = cx
            .new(|cx| branch_diff::BranchDiff::new(DiffBase::Staged, project.clone(), window, cx));
        let focus_handle = cx.focus_handle();
        let multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(Capability::ReadOnly);
            multibuffer.set_all_diff_hunks_expanded(cx);
            multibuffer
        });

        let editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(multibuffer.clone(), Some(project.clone()), window, cx);
            editor.start_temporary_diff_override();
            editor.disable_diagnostics(cx);
            editor.set_expand_all_diff_hunks(cx);
            editor.set_render_diff_hunk_controls(
                Arc::new(|_, _, _, _, _, _, _, _| gpui::Empty.into_any_element()),
                cx,
            );
            editor
        });

        let editor_subscription = cx.subscribe_in(&editor, window, Self::handle_editor_event);
        let branch_diff_subscription = cx.subscribe_in(
            &branch_diff,
            window,
            move |this, _, event, window, cx| match event {
                BranchDiffEvent::FileListChanged => {
                    this._task = window.spawn(cx, {
                        let this = cx.weak_entity();
                        async |cx| Self::refresh(this, RefreshReason::StatusesChanged, cx).await
                    })
                }
            },
        );

        let task = window.spawn(cx, {
            let this = cx.weak_entity();
            async |cx| Self::refresh(this, RefreshReason::StatusesChanged, cx).await
        });

        Self {
            project,
            multibuffer,
            branch_diff,
            editor,
            buffer_diff_subscriptions: HashMap::default(),
            focus_handle,
            pending_scroll: None,
            _task: task,
            _subscription: Subscription::join(editor_subscription, branch_diff_subscription),
        }
    }

    fn move_to_entry(
        &mut self,
        entry: GitStatusEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path_key = PathKey::with_sort_prefix(0, entry.repo_path.as_ref().clone());
        self.move_to_path(path_key, window, cx);
    }

    fn move_to_path(&mut self, path_key: PathKey, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(position) = self.multibuffer.read(cx).location_for_path(&path_key, cx) {
            self.editor.update(cx, |editor, cx| {
                editor.change_selections(
                    SelectionEffects::scroll(Autoscroll::focused()),
                    window,
                    cx,
                    |s| {
                        s.select_ranges([position..position]);
                    },
                )
            });
        } else {
            self.pending_scroll = Some(path_key);
        }
    }

    fn handle_editor_event(
        &mut self,
        editor: &Entity<Editor>,
        _event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if editor.focus_handle(cx).contains_focused(window, cx)
            && self.multibuffer.read(cx).is_empty()
        {
            self.focus_handle.focus(window, cx)
        }
    }

    fn register_buffer(
        &mut self,
        path_key: PathKey,
        buffer: Entity<Buffer>,
        diff: Entity<BufferDiff>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(&diff, window, move |this, _, _, window, cx| {
            this._task = window.spawn(cx, {
                let this = cx.weak_entity();
                async |cx| Self::refresh(this, RefreshReason::DiffChanged, cx).await
            })
        });
        self.buffer_diff_subscriptions
            .insert(path_key.path.clone(), (diff.clone(), subscription));

        let snapshot = buffer.read(cx).snapshot();
        let excerpt_ranges = diff
            .read(cx)
            .snapshot(cx)
            .hunks_intersecting_range(
                Anchor::min_max_range_for_buffer(snapshot.remote_id()),
                &snapshot,
            )
            .map(|diff_hunk| diff_hunk.buffer_range.to_point(&snapshot))
            .collect::<Vec<_>>();

        let was_empty = self.multibuffer.read(cx).is_empty();
        self.multibuffer.update(cx, |multibuffer, cx| {
            multibuffer.set_excerpts_for_path(
                path_key.clone(),
                buffer,
                excerpt_ranges,
                multibuffer_context_lines(cx),
                cx,
            );
            multibuffer.add_diff(diff, cx);
        });

        if was_empty {
            self.editor.update(cx, |editor, cx| {
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                    selections
                        .select_ranges([multi_buffer::Anchor::min()..multi_buffer::Anchor::min()])
                });
            });
        }

        if self.pending_scroll.as_ref() == Some(&path_key) {
            self.move_to_path(path_key, window, cx);
        }
    }

    #[instrument(skip_all)]
    async fn refresh(
        this: WeakEntity<Self>,
        reason: RefreshReason,
        cx: &mut AsyncWindowContext,
    ) -> Result<()> {
        let mut path_keys = Vec::new();
        let buffers_to_load = this.update(cx, |this, cx| {
            let (repo, buffers_to_load) = this.branch_diff.update(cx, |branch_diff, cx| {
                let load_buffers = branch_diff.load_buffers(cx);
                (branch_diff.repo().cloned(), load_buffers)
            });
            let mut previous_paths = this
                .multibuffer
                .read(cx)
                .paths()
                .cloned()
                .collect::<HashSet<_>>();

            if let Some(repo) = repo {
                path_keys = Vec::with_capacity(buffers_to_load.len());
                for entry in &buffers_to_load {
                    let path_key = PathKey::with_sort_prefix(0, entry.repo_path.as_ref().clone());
                    previous_paths.remove(&path_key);
                    path_keys.push(path_key);
                }

                let _ = repo;
            }

            for path in previous_paths {
                if let Some(buffer) = this.multibuffer.read(cx).buffer_for_path(&path, cx) {
                    let skip =
                        matches!(reason, RefreshReason::DiffChanged) && buffer.read(cx).is_dirty();
                    if skip {
                        continue;
                    }
                }

                this.buffer_diff_subscriptions.remove(&path.path);
                this.multibuffer.update(cx, |multibuffer, cx| {
                    if let Some(buffer) = multibuffer.buffer_for_path(&path, cx) {
                        multibuffer.remove_excerpts_for_buffer(buffer.read(cx).remote_id(), cx);
                    }
                });
            }

            buffers_to_load
        })?;

        for (entry, path_key) in buffers_to_load.into_iter().zip(path_keys.into_iter()) {
            if let Some((buffer, diff)) = entry.load.await.log_err() {
                cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        let multibuffer = this.multibuffer.read(cx);
                        let skip = multibuffer.buffer(buffer.read(cx).remote_id()).is_some()
                            && multibuffer
                                .diff_for(buffer.read(cx).remote_id())
                                .is_some_and(|prev_diff| prev_diff.entity_id() == diff.entity_id())
                            && matches!(reason, RefreshReason::DiffChanged)
                            && buffer.read(cx).is_dirty();
                        if !skip {
                            this.register_buffer(path_key, buffer, diff, window, cx);
                        }
                    })
                    .ok();
                })?;
            }
        }

        this.update(cx, |this, cx| {
            this.pending_scroll.take();
            cx.notify();
        })?;

        Ok(())
    }
}

impl EventEmitter<EditorEvent> for StagedDiff {}

impl Focusable for StagedDiff {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.multibuffer.read(cx).is_empty() {
            self.focus_handle.clone()
        } else {
            self.editor.focus_handle(cx)
        }
    }
}

impl Item for StagedDiff {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitBranch).color(Color::Muted))
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.deactivated(window, cx);
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some("Staged Changes".into())
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Staged Changes".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Git Staged Diff Opened")
    }

    fn as_searchable(&self, _: &Entity<Self>, _cx: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.editor.read(cx).for_each_project_item(cx, f)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn can_save(&self, _: &App) -> bool {
        false
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<workspace::WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>>
    where
        Self: Sized,
    {
        let project = self.project.clone();
        Task::ready(Some(cx.new(|cx| Self::new(project, window, cx))))
    }

    fn save(
        &mut self,
        _: SaveOptions,
        _: Entity<Project>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.clone().into())
        } else if type_id == TypeId::of::<branch_diff::BranchDiff>() {
            Some(self.branch_diff.clone().into())
        } else {
            None
        }
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }
}

impl SerializableItem for StagedDiff {
    fn serialized_item_kind() -> &'static str {
        "StagedDiff"
    }

    fn cleanup(
        _: workspace::WorkspaceId,
        _: Vec<workspace::ItemId>,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn deserialize(
        project: Entity<Project>,
        _: WeakEntity<Workspace>,
        _: workspace::WorkspaceId,
        _: workspace::ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        Task::ready(Ok(cx.new(|cx| Self::new(project, window, cx))))
    }

    fn serialize(
        &mut self,
        _: &mut Workspace,
        _: workspace::ItemId,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        Some(Task::ready(Ok(())))
    }

    fn should_serialize(&self, _: &Self::Event) -> bool {
        false
    }
}

impl Render for StagedDiff {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.multibuffer.read(cx).is_empty();

        div()
            .track_focus(&self.focus_handle)
            .key_context(if is_empty { "EmptyPane" } else { "GitDiff" })
            .bg(cx.theme().colors().editor_background)
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .when(is_empty, |el| {
                el.child(
                    v_flex().gap_1().child(
                        h_flex()
                            .justify_around()
                            .child(Label::new("No staged changes")),
                    ),
                )
            })
            .when(!is_empty, |el| el.child(self.editor.clone()))
    }
}
