# Worktree picker / title bar review handoff

## Summary

I reviewed the `worktree-picker-title-bar` diff end to end.

The refactor is directionally strong:

- worktree actions move out of `agent_ui` and into `git_ui` / `zed_actions`
- worktree handling is centralized in `git_ui::worktree_service`
- `MultiWorkspace` now carries an optional `source_workspace` through activation
- the title bar gets cleaner separation between worktree selection and branch/stash selection
- branch icon settings are renamed to match the new status-icon behavior

That said, I would block on two regressions and flag two follow-up issues.

## Main findings

### 1. Switching to an existing worktree appears to lose prior workspace state

This looks like the biggest regression.

In the old `agent_ui` flow, both create and switch paths went through the same workspace-opening logic, which restored state like:

- dock structure
- remapped open files / active file
- agent-thread initialization from the source workspace

In the new `git_ui::worktree_service` flow, that inheritance is gated to `WorktreeOperation::Create`.

Files to inspect:

- `crates/git_ui/src/worktree_service.rs`
- `crates/workspace/src/multi_workspace.rs`
- `crates/zed/src/zed.rs`
- `crates/agent_ui/src/agent_panel.rs`

The key issue is in `open_worktree_workspace`:

- `source_for_transfer` is only set for `Create`
- the `init` closure that restores dock structure is only set for `Create`
- the file remapping / reopen logic is only run for `Create`
- focused-dock restoration is only run for `Create`

That means `SwitchWorktree` now looks materially weaker than the old behavior.

I would verify this specifically by switching to an existing linked worktree while:

- several files are open
- a dock panel is focused
- the agent panel contains draft text

Expected based on old behavior: those should transfer.
Current implementation suggests they will not.

### 2. Switching to an existing worktree now runs create-worktree hooks

`worktree_service::open_worktree_workspace` calls `workspace.run_create_worktree_tasks(window, cx)` unconditionally in the shared path.

But `Workspace::run_create_worktree_tasks` is specifically keyed off `TaskHook::CreateWorktree`.

Files to inspect:

- `crates/git_ui/src/worktree_service.rs`
- `crates/workspace/src/tasks.rs`

This means a plain switch/open of an existing worktree now appears to trigger create-worktree tasks, which is likely wrong and user-visible for anyone relying on task hooks.

### 3. The settings migration is too shallow

The new migrator only renames the direct nested key under root-level `title_bar`.

Files to inspect:

- `crates/migrator/src/migrations/m_2026_04_17/settings.rs`
- `crates/migrator/src/patterns/settings.rs`
- `crates/migrator/src/migrations.rs`

The query used is `SETTINGS_NESTED_KEY_VALUE_PATTERN`, which only matches a single nested object shape like:

- `title_bar.show_branch_icon`

It does not appear to reach nested override scopes such as:

- platform overrides
- release-channel overrides
- profile settings objects

That matters because the repo already has helper logic for applying setting migrations across root plus override/profile scopes.

So the runtime has moved fully to `show_branch_status_icon`, but some persisted nested `show_branch_icon` keys may remain on disk and silently stop having any effect.

I ran:

- `cargo test -p migrator test_rename_show_branch_icon_to_show_branch_status_icon`

The test passed, but it only covers the root-level case and does not cover nested override/profile cases.

### 4. The rename is not behavior-preserving

This is not just a key rename; it changes semantics.

Old behavior:

- `title_bar.show_branch_icon = false` hid the branch icon entirely

New behavior:

- `title_bar.show_branch_status_icon = false` still shows a neutral branch icon
- it only disables status-specific icon variants

Files to inspect:

- old title bar behavior from `origin/main`: `crates/title_bar/src/title_bar.rs`
- new behavior: `crates/title_bar/src/title_bar.rs`
- settings/schema/docs:
  - `crates/settings_content/src/title_bar.rs`
  - `crates/title_bar/src/title_bar_settings.rs`
  - `crates/settings_ui/src/page_data.rs`
  - `assets/settings/default.json`
  - `docs/src/reference/all-settings.md`
  - `docs/src/visual-customization.md`

If this change is intentional, it should probably be treated as a new setting or a deprecation with migration notes, not a simple rename migration.

## Minor UX issue

The new worktree picker modal can show enabled-looking footer actions for selections that are actually no-ops.

Examples to check:

- disabled create-named entries still select, and the footer still shows `Create`
- the current worktree still gets an `Open` footer button even though confirm is intentionally a no-op there

Files to inspect:

- `crates/git_ui/src/worktree_picker.rs`

This is not as severe as the state-transfer issue, but it will feel broken in use.

## Optimized review path

If you want to re-review the patch efficiently, I would do it in this order:

### A. Start with the new action surface

Files:

- `crates/zed_actions/src/lib.rs`
- `crates/git_ui/src/git_ui.rs`

What changed:

- `CreateWorktree`, `SwitchWorktree`, and `OpenWorktreeInNewWindow` now live in `zed_actions`
- `git_ui` owns the worktree entrypoints
- `git::Worktree` now opens a dedicated `WorktreePicker`

Why it matters:

- this is the architectural handoff from `agent_ui` to `git_ui`
- all later behavior differences stem from this move

### B. Then read the shared worktree service carefully

File:

- `crates/git_ui/src/worktree_service.rs`

Focus on:

- `resolve_worktree_branch_target`
- `handle_create_worktree`
- `handle_switch_worktree`
- `open_worktree_workspace`

Why it matters:

- this is the heart of the refactor
- the two biggest regressions both live here

### C. Then read the workspace activation plumbing

Files:

- `crates/workspace/src/multi_workspace.rs`
- `crates/workspace/src/workspace.rs`
- `crates/zed/src/zed.rs`
- `crates/agent_ui/src/agent_panel.rs`

Focus on:

- `MultiWorkspaceEvent::ActiveWorkspaceChanged { source_workspace }`
- `find_or_create_workspace_with_source_workspace`
- `Workspace::capture_state_for_worktree_switch`
- `AgentPanel::initialize_from_source_workspace_if_needed`

Why it matters:

- this is where the draft-transfer / source-workspace inheritance model lives now
- it is also where the create vs. switch asymmetry becomes visible

### D. Then review the new picker UX

Files:

- `crates/git_ui/src/worktree_picker.rs`
- `crates/git_ui/src/git_picker.rs`
- `crates/title_bar/src/title_bar.rs`

What changed:

- worktrees are no longer a tab in `GitPicker`
- a dedicated `WorktreePicker` now backs both the modal and title bar popover
- title bar now separates worktree selection from branch/stash selection

Why it matters:

- this is the main user-facing part of the patch
- the footer-action UX mismatch lives here

### E. Finish with settings/migration/docs

Files:

- `crates/migrator/src/migrations/m_2026_04_17/settings.rs`
- `crates/settings_content/src/title_bar.rs`
- `crates/title_bar/src/title_bar_settings.rs`
- `crates/settings_ui/src/page_data.rs`
- docs files touched in `docs/src`

Why it matters:

- the code and docs are internally consistent with the new name
- the migration and semantics are where the remaining risk is

## Suggested fixes

### For finding 1

Make `SwitchWorktree` inherit the same source-workspace state that the old flow did, or explicitly decide which parts should carry forward and document the behavioral change.

At minimum, I would re-evaluate whether switch should preserve:

- source agent draft
- dock structure
- open-file remapping
- focused-dock restoration

### For finding 2

Only call `run_create_worktree_tasks()` for `WorktreeOperation::Create`.

### For finding 3

Extend the migration so it also rewrites nested override/profile scopes, not just the root `title_bar` object.

### For finding 4

If the semantics change is intentional, avoid presenting it as a pure rename migration.

Possible options:

- keep backward compatibility for `show_branch_icon`
- introduce `show_branch_status_icon` as a new setting without auto-renaming
- document the semantic change more explicitly in release notes / migration notes

## Commands run

- `cargo test -p migrator test_rename_show_branch_icon_to_show_branch_status_icon`

## Bottom line

I like the architecture shift overall, but I would not merge this as-is without resolving:

1. switch-path state transfer / draft inheritance
2. create hooks firing on switch

I would also want the settings migration behavior clarified before ship.
