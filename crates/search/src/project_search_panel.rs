use std::sync::Arc;

use anyhow::Result;
use editor::EditorSettings;
use gpui::{
    Action, App, AppContext as _, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Pixels, Render, Styled, Subscription, WeakEntity,
    Window, div, px,
};
use project::{Fs, Project};
use settings::{DockSide, ProjectSearchMode, Settings};
use workspace::{
    NewFile, Pane, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::project_search::{
    ProjectSearch, ProjectSearchBar, ProjectSearchView, TogglePanel, TogglePanelFocus,
};

const PROJECT_SEARCH_PANEL_KEY: &str = "ProjectSearchPanel";

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _cx: &mut Context<Workspace>| {
            workspace.register_action(|workspace, _: &TogglePanelFocus, window, cx| {
                workspace.toggle_panel_focus::<ProjectSearchPanel>(window, cx);
            });
            workspace.register_action(|workspace, _: &TogglePanel, window, cx| {
                if !workspace.toggle_panel_focus::<ProjectSearchPanel>(window, cx) {
                    workspace.close_panel::<ProjectSearchPanel>(window, cx);
                }
            });
        },
    )
    .detach();
}

pub struct ProjectSearchPanel {
    pane: Entity<Pane>,
    fs: Arc<dyn Fs>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    _subscriptions: Vec<Subscription>,
}

impl ProjectSearchPanel {
    fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = workspace.project();
        let weak_workspace = workspace.weak_handle();

        let pane = cx.new(|cx| {
            let mut pane = Pane::new(
                weak_workspace.clone(),
                project.clone(),
                Default::default(),
                None,
                NewFile.boxed_clone(),
                false,
                window,
                cx,
            );
            pane.set_can_navigate(false, cx);
            pane.display_nav_history_buttons(None);
            pane.set_should_display_tab_bar(|_, _| false);
            pane.set_zoom_out_on_close(false);
            pane.set_can_split(Some(Arc::new(|_, _, _, _| false)));

            let toolbar = pane.toolbar().clone();
            let project_search_bar = cx.new(|_| ProjectSearchBar::new());
            toolbar.update(cx, |toolbar, cx| {
                toolbar.add_item(project_search_bar, window, cx);
            });

            pane
        });

        let subscriptions = vec![cx.subscribe(&pane, Self::handle_pane_event)];

        let mut panel = Self {
            pane,
            fs: workspace.app_state().fs.clone(),
            workspace: weak_workspace,
            project: project.clone(),
            _subscriptions: subscriptions,
        };
        panel.ensure_view(window, cx);
        panel
    }

    fn ensure_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_view = self
            .pane
            .read(cx)
            .items()
            .any(|item| item.downcast::<ProjectSearchView>().is_some());
        if has_view {
            return;
        }

        let project_search = cx.new(|cx| ProjectSearch::new(self.project.clone(), cx));
        let workspace = self.workspace.clone();
        let search_view = cx.new(|cx| {
            ProjectSearchView::new(workspace, project_search, window, cx, None)
        });
        search_view.update(cx, |view, cx| {
            view.set_open_results_in_center_pane(true, cx);
        });
        self.pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(search_view), false, false, None, window, cx);
        });
    }

    fn handle_pane_event(
        &mut self,
        _: Entity<Pane>,
        event: &workspace::pane::Event,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, workspace::pane::Event::Remove { .. }) {
            cx.emit(PanelEvent::Close);
        }
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            cx.new(|cx| Self::new(workspace, window, cx))
        })
    }

    pub(crate) fn pane_entity(&self) -> Entity<Pane> {
        self.pane.clone()
    }
}

impl EventEmitter<PanelEvent> for ProjectSearchPanel {}

impl Focusable for ProjectSearchPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.pane.focus_handle(cx)
    }
}

impl Render for ProjectSearchPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.pane.clone())
    }
}

impl Panel for ProjectSearchPanel {
    fn persistent_name() -> &'static str {
        "ProjectSearchPanel"
    }

    fn panel_key() -> &'static str {
        PROJECT_SEARCH_PANEL_KEY
    }

    fn position(&self, _window: &Window, cx: &App) -> DockPosition {
        match EditorSettings::get_global(cx)
            .search
            .project_search_panel_dock
        {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dock) = (match position {
            DockPosition::Left => Some(DockSide::Left),
            DockPosition::Right => Some(DockSide::Right),
            DockPosition::Bottom => None,
        }) else {
            return;
        };

        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            settings
                .editor
                .search
                .get_or_insert_default()
                .project_search_panel_dock = Some(dock);
        });
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(420.)
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<ui::IconName> {
        let search = EditorSettings::get_global(cx).search;
        if !search.button || search.project_search_mode != ProjectSearchMode::Panel {
            return None;
        }
        Some(ui::IconName::MagnifyingGlass)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Project Search Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(TogglePanelFocus)
    }

    fn pane(&self) -> Option<Entity<Pane>> {
        Some(self.pane.clone())
    }

    fn activation_priority(&self) -> u32 {
        4
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.ensure_view(window, cx);
        }
    }
}
