# Worktree picker / title bar review handoff

## Summary

I reviewed the `worktree-picker-title-bar` diff end to end.

The refactor is directionally strong:

- worktree actions move out of `agent_ui` and into `git_ui` / `zed_actions`
- worktree handling is centralized in `git_ui::worktree_service`
- `MultiWorkspace` now carries an optional `source_workspace` through activation
- the title bar gets cleaner separation between worktree selection and branch/stash selection
- branch icon settings are renamed to match the new status-icon behavior

That said, if switching to an existing worktree is not a priority, I would focus this review on the settings migration issue first, then the semantics change around the renamed title-bar setting, and finally the smaller picker UX follow-up.

## Main findings

### 1. The settings migration is too shallow

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

### 2. The rename is not behavior-preserving

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

### 3. Minor UX issue

The new worktree picker modal can show enabled-looking footer actions for selections that are actually no-ops.

Examples to check:

- disabled create-named entries still select, and the footer still shows `Create`
- the current worktree still gets an `Open` footer button even though confirm is intentionally a no-op there

Files to inspect:

- `crates/git_ui/src/worktree_picker.rs`

This is lower severity than the settings migration issue, but it will feel broken in use.

## Optimized review path

If you want to re-review the patch efficiently, I would do it in this order:

### A. Start with the title-bar settings rename and migration

Files:

- `crates/migrator/src/migrations/m_2026_04_17/settings.rs`
- `crates/migrator/src/patterns/settings.rs`
- `crates/migrator/src/migrations.rs`
- `crates/settings_content/src/title_bar.rs`
- `crates/title_bar/src/title_bar_settings.rs`
- `crates/settings_ui/src/page_data.rs`

What changed:

- `show_branch_icon` was renamed to `show_branch_status_icon`
- runtime settings, UI, defaults, and docs now consistently use the new key
- the migrator attempts to rewrite persisted settings

Why it matters:

- this is the clearest user-facing risk in the patch if you do not care about existing-worktree switching
- the migration does not appear to cover nested override/profile scopes
- the rename also changes semantics, not just the key name

### B. Then review the new action surface

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

### C. Then read the shared worktree service carefully

File:

- `crates/git_ui/src/worktree_service.rs`

Focus on:

- `resolve_worktree_branch_target`
- `handle_create_worktree`
- `handle_switch_worktree`
- `open_worktree_workspace`

Why it matters:

- this is the heart of the refactor
- even if switch behavior is deprioritized, this is still where the main worktree lifecycle decisions now live

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

### E. Optional: inspect workspace/source-workspace plumbing if switch behavior matters later

Files:

- `crates/workspace/src/multi_workspace.rs`
- `crates/workspace/src/workspace.rs`
- `crates/zed/src/zed.rs`
- `crates/agent_ui/src/agent_panel.rs`

Why it matters:

- this is where the draft-transfer / source-workspace inheritance model lives now
- I am explicitly deprioritizing this because you said you do not care about switching to an existing worktree

## Suggested fixes

### For finding 1

Extend the migration so it also rewrites nested override/profile scopes, not just the root `title_bar` object.

### For finding 2

If the semantics change is intentional, avoid presenting it as a pure rename migration.

Possible options:

- keep backward compatibility for `show_branch_icon`
- introduce `show_branch_status_icon` as a new setting without auto-renaming
- document the semantic change more explicitly in release notes / migration notes

## Commands run

- `cargo test -p migrator test_rename_show_branch_icon_to_show_branch_status_icon`

## Bottom line

Given your priority, I would focus review energy on:

1. the shallow settings migration
2. the fact that the rename is not behavior-preserving
3. the smaller worktree picker UX mismatch

I like the architecture shift overall, and those are the pieces I would want clarified before ship.
