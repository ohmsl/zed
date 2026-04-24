use anyhow::{Context as _, Result};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, SharedString, Subscription,
    Task, WeakEntity,
};
use project::{
    AgentId, Project,
    agent_server_store::{AgentServerCommand, ExternalAgentTerminalRequest},
};
use task::{HideStrategy, RevealStrategy, SpawnInTerminal, TaskId};
use terminal_view::TerminalView;
use ui::prelude::*;
use workspace::{PathList, Workspace};

use crate::{Agent, ThreadId};

pub enum TerminalAgentViewEvent {
    Loaded,
}

impl EventEmitter<TerminalAgentViewEvent> for TerminalAgentView {}

pub struct TerminalAgentView {
    thread_id: ThreadId,
    agent: Agent,
    agent_session_id: SharedString,
    title: Option<SharedString>,
    work_dirs: PathList,
    terminal_view: Option<Entity<TerminalView>>,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    project: WeakEntity<Project>,
    _subscriptions: [Subscription; 1],
}

impl TerminalAgentView {
    pub fn new(
        thread_id: ThreadId,
        agent: Agent,
        agent_session_id: SharedString,
        title: Option<SharedString>,
        work_dirs: PathList,
        workspace: WeakEntity<Workspace>,
        project: WeakEntity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscription = cx.on_focus(&focus_handle, window, |this, window, cx| {
            if let Some(terminal_view) = this.terminal_view.as_ref() {
                terminal_view.focus_handle(cx).focus(window, cx);
            }
        });

        Self {
            thread_id,
            agent,
            agent_session_id,
            title,
            work_dirs,
            terminal_view: None,
            focus_handle,
            workspace,
            project,
            _subscriptions: [focus_subscription],
        }
    }

    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn title(&self) -> SharedString {
        self.title.clone().unwrap_or_else(|| self.agent.label())
    }

    pub fn terminal_view(&self) -> Option<&Entity<TerminalView>> {
        self.terminal_view.as_ref()
    }

    pub fn launch_new_session(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.launch_terminal(
            ExternalAgentTerminalRequest::NewSession {
                session_id: self.agent_session_id.clone(),
            },
            window,
            cx,
        )
    }

    pub fn resume_session(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.launch_terminal(
            ExternalAgentTerminalRequest::ResumeSession {
                session_id: self.agent_session_id.clone(),
            },
            window,
            cx,
        )
    }

    fn launch_terminal(
        &mut self,
        request: ExternalAgentTerminalRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(project) = self.project.upgrade() else {
            return Task::ready(Err(anyhow::anyhow!("project no longer exists")));
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow::anyhow!("workspace no longer exists")));
        };

        let work_dirs = self.work_dirs.clone();
        let agent_id = self.agent.id();
        let title = self.title();
        let project_for_command = project.clone();
        let command_task = project.update(cx, |project, cx| {
            let store = project.agent_server_store().clone();
            let mut async_cx = cx.to_async();
            store.update(cx, |store, _| {
                store
                    .terminal_command(&agent_id, request, Default::default(), &mut async_cx)
                    .context("external agent is not registered for terminal launch")
            })
        });

        let workspace_weak = workspace.downgrade();
        let project_weak = project.downgrade();

        cx.spawn_in(window, async move |this, cx| {
            let command_task = command_task?;
            let command = command_task.await?;
            let spawn_task = build_spawn_in_terminal(&agent_id, &title, &work_dirs, &command);
            let terminal_task = project_for_command.update(cx, |project, cx| {
                project.create_terminal_task(spawn_task, cx)
            });
            let terminal = terminal_task.await?;

            let terminal_view = cx.new_window_entity(|window, cx| {
                let mut terminal_view = TerminalView::new(
                    terminal,
                    workspace_weak.clone(),
                    None,
                    project_weak.clone(),
                    window,
                    cx,
                );
                terminal_view.set_embedded_mode(Some(1000), cx);
                terminal_view
            })?;

            this.update_in(cx, |this, _window, cx| {
                this.terminal_view = Some(terminal_view);
                cx.emit(TerminalAgentViewEvent::Loaded);
                cx.notify();
            })?;

            anyhow::Ok(())
        })
    }
}

fn build_spawn_in_terminal(
    agent_id: &AgentId,
    title: &SharedString,
    work_dirs: &PathList,
    command: &AgentServerCommand,
) -> SpawnInTerminal {
    SpawnInTerminal {
        id: TaskId(format!("agent-terminal:{}:{}", agent_id, title)),
        full_label: title.to_string(),
        label: title.to_string(),
        command_label: command.args.iter().fold(
            command.path.to_string_lossy().to_string(),
            |mut label, arg| {
                label.push(' ');
                label.push_str(arg);
                label
            },
        ),
        command: Some(command.path.to_string_lossy().to_string()),
        args: command.args.clone(),
        cwd: work_dirs.paths().first().cloned(),
        env: command.env.clone().unwrap_or_default(),
        use_new_terminal: true,
        allow_concurrent_runs: true,
        reveal: RevealStrategy::Always,
        hide: HideStrategy::Never,
        ..Default::default()
    }
}

impl Focusable for TerminalAgentView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalAgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .children(self.terminal_view.clone())
    }
}
