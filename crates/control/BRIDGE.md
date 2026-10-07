<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Control bridge spec (protocol v3)

Owner: the mcp-v3 teammate (crates/control). Implementer: ui-polish, in
`crates/ui/src/control_bridge.rs`. This file says, for every
`protocol::control::RequestBody` variant, which session or document
operation the ui bridge must call, what it replies, and what activity and
glow it reports. The MCP tools in `crates/control/src/mcp` are built on
exactly these semantics.

An executable version of this spec is the reference bridge in
`crates/control/tests/support/mod.rs` (`RefDaw::answer`). It runs on the
real `doc::history::Editor` and every MCP end-to-end test goes through it.
When this file and that code disagree, the code is what the tests prove;
tell the mcp-v3 owner.

## Rules for every request

1. **Same path as the window.** Document changes go through the same
   `Session` method the widgets use (`Session::submit` for edit batches,
   `Session::undo`/`redo`, and the branch wrappers below), so slot sync,
   plugin capture, recompile and autosave happen exactly as for the user.
   Never apply an `Edit` to the document directly.
2. **Author.** `author_of(&inc.client)`: `Author::Script` for scripts,
   `Author::Agent("<client name>-<client id>")` for agents. Every commit the
   request makes carries that author. Undo and redo for a client use
   `Scope::Only(author)`; the user's own undo uses `Scope::Any`.
3. **Strict staleness.** For `Edit`, `KitAdd`, `BranchSwitch` and
   `VersionRestore`: if `base_revision` is `Some(b)` and `b < revision`,
   reply `Stale { current: revision }` and change nothing. Keep this rule
   strict (any newer revision is stale): `control` predicts the ids a batch
   creates from the project it read and relies on nothing being allocated
   in between (`crates/control/src/mcp/ids.rs`).
4. **Ids.** Batches go through `doc::document::apply_batch_indexed` (inside
   `Editor::submit`) unchanged: ids in `Applied::created` in edit order,
   new contents before new clips inside one clip edit, the channel before
   the instance for a CLAP instrument, notes of copied content not
   reported. `control` depends on this order.
5. **Gestures.** A batch submitted while a gesture is open is queued
   (`Submitted::Queued`): call `server.defer(ticket)` and answer from
   `on_done` (as today). Branch switches, restores and undo during a
   gesture reply `Busy`.
6. **Revisions.** Call `ControlServer::notify_revision(revision)` after
   every change of `document().revision`, whoever caused it (edit, undo,
   redo, branch switch, restore, load, a plugin's own parameter change).
   Agents subscribed to `libredaw://project` and `libredaw://history`
   depend on it.
7. **Errors.** `EditFailure { index, error }` becomes
   `ControlError::Edit { index, error }` with the index into the request's
   batch (control maps it back to the part of the tool call).
   `HistoryError::GestureOpen` and `BranchError::GestureOpen` become
   `Busy`; other `HistoryError`s become `BadRequest { reason: e.to_string() }`.
   `BranchError::UnknownBranch(b)` becomes `NotFound { what: "branch <b>" }`,
   `UnknownCommit(c)` becomes `NotFound { what: "commit <c>" }`, `BadName`
   and `IsCurrent` become `BadRequest` with the error text.
8. **PRIVILEGED.** `RequestBody::privileged(dirty)` as today, plus: an
   `Edit` batch naming a plugin not yet approved for agents (existing
   `plugins_in`), and `VersionRestore` from an agent (SPEC 17.1). Call
   `require_approval(ticket, summary)` with a sentence such as
   "restore the version First beat"; run the request on Allow.
9. **Untrusted text.** Names, tags and descriptions that go back to a
   client pass through `agent_string` (control cleans them again).

## Session wrappers ui-polish must add

`Session::undo`/`redo` already run `after_change`. The branch operations
replace the document too, so they need the same treatment:

```rust
impl Session {
    pub fn create_branch(&mut self, author: &Author, name: &str, from: Option<&str>) -> Result<String, BranchError>;
    pub fn switch_branch(&mut self, key: &str) -> Result<(), BranchError>;
    pub fn undo_switch(&mut self) -> bool;
    pub fn restore_version(&mut self, author: Author, commit: &str) -> Result<Applied, BranchError>;
}
// each: let old = project.clone(); call self.editor.<same>; on success self.after_change(&old, Origin::External)
```

The Versions panel (15.12) uses the same wrappers, so an agent's branch
and the user's branch are the same thing.

## Requests

| Request | Session / doc operation | Reply | Activity and glow |
|---|---|---|---|
| `ProjectGet` | `session.document()` | `Project { revision, project }` | none |
| `ProjectInfo` | path, tempo, mtime, `is_dirty()` | `ProjectInfo` | none |
| `ProjectList` | projects folder listing (as today) | `Projects` | none |
| `ProjectNew { template }` | PRIVILEGED if dirty (autosave first); `files::fresh_project` or the template of that name (Home "Start a Beat" cards) | `Done` | ends the agent's activity |
| `ProjectOpen { path }` | path must be inside the projects folder, else `NotFound`; PRIVILEGED if dirty; `files::open_path` | `Done` | ends the activity |
| `ProjectSave` | `files::save_current`, reply from its callback | `Done` or `Internal { reason }` | none |
| `Edit { edits }` | rule 3, then `Session::submit(author, None, edits, token)` | `Applied { revision, created }`, or later from `on_done` | adds the commit to the open activity's group; glows the objects the batch touched (see "Glow") |
| `NotesList { pattern }` | `project.pattern(pattern)` | `Notes { notes }` (all notes of that content) or `NotFound { what: "content <id>" }` | none |
| `Play` | the transport's play (timeline; the loop region repeats when enabled) | `Done` | none |
| `Stop` | the transport's stop (first stop returns to the run start) | `Done` | none |
| `TransportState` | playing flag, playhead tick, `project.tempo_bpm`, `project.loop_region` | `Transport { playing, tick, tempo_bpm, loop_region }` | none |
| `Undo` / `Redo` | `Session::undo(&scope_of(author))` / `redo` | `Applied { revision, created: [] }` (control also accepts `Done`) | glows what the undone commit touched, briefly |
| `History` | `editor.history().infos()` | `History { entries }` | none |
| `HistoryTree { since, limit }` | `editor.history_nodes(since, limit)`, `editor.head_name()` | `HistoryTree { head, nodes }` (newest first; `limit` 0 = all) | none |
| `HistoryDiff { from, to }` | `editor.diff(from, to)`; for commits no longer in memory `HistoryStore::load_project` plus `doc::diff::diff_projects` | `HistoryDiff { lines }` | none |
| `VersionSave { name }` | `editor.save_version(name)` | `Done` | none |
| `BranchCreate { name, from }` | `Session::create_branch(&author, name, from)`; `from` is a commit name (control resolves branch names to their head first) | `Branches { current, branches }` (`branch_infos()`); the new branch is current | the Versions panel shows the new card; the agent glow marks it while the agent edits it |
| `BranchSwitch { branch }` | rule 3, then `Session::switch_branch(branch)` (id or display name). While playing, apply it at the next bar and keep the playhead (15.12), then reply | `Applied { revision, created: [] }` | none |
| `BranchList` | `editor.current_branch()`, `branch_infos()` | `Branches { current, branches }` | none |
| `BranchRename { branch, name }` | `editor.rename_branch` | `Done` | none |
| `BranchArchive { branch }` | `editor.archive_branch` | `Done` | none |
| `VersionRestore { commit }` | rule 3; PRIVILEGED for agents; `Session::restore_version(author, commit)` | `Applied { revision, created: [] }` | adds the commit to the activity group |
| `ExportWav { format, start, end, tail_seconds }` | export job on the pinned revision; range `start..end` ticks, default the loop region if enabled, else `0..` end of the last clip; plus `tail_seconds`; file in the exports folder | `Job { job, revision }` | none |
| `Analyze { start, end }` | the same range as `ExportWav` without a tail; `control::analysis::analyze` on the export thread | `Job { job, revision }`; `JobResult` gives `Analysis` with `revision` set | none |
| `JobStatus` / `JobResult` / `JobCancel` | as today | `JobStatus`, `Exported` or `Analysis`, `Done` | none |
| `SettingsGet` / `SettingsSet` | as today (closed `Setting` enum) | `Settings` / `Done` | none |
| `PluginScan` / `PluginList` | as today | `Done` / `Plugins` (with `agent_approved`) | none |
| `SetActivity { text, focus }` | pill text (cleaned, at most 80 chars, screen reader announcement); `focus` glows that object; `text: None` ends the activity and its commit group | `Done` | see "Activity groups" |
| `SoundSearch { role, genre, tags, limit }` | the sound library query of the browser; `limit` capped at 50 | `Sounds { sounds }` | none |
| `KitAdd { pack, kit, track }` | rule 3; what the browser's "add kit" does: one sampler instrument per kit piece (sample registered first), on `track` or one new track named after the kit, in ONE `Session::submit` | `Applied { revision, created }`: the new track first (if any), then the instruments | glows the new instruments |

## Glow (SPEC 18.1, Amendment 18)

While an agent is active the window has the orange edge glow. The objects
it changes glow too, derived from each agent batch (or the explicit
`SetActivity` focus, which wins):

| Edit kinds | Glowing object |
|---|---|
| `AddChannel`, `RenameChannel`, `SetChannel*`, `SetRootKey`, `SetSynth*`, `SetSampler*`, `SetBass808*`, `SetChokeGroup` | that instrument row (`Focus::Channel`) |
| `AddClip`, `DuplicateClips`, `MoveClips`, `MoveClipToInstrument`, `ResizeClips`, `SplitClip`, `MakeUnique`, `SetClipMuted` | those clips (`Focus::Clip`), new ones from `created` |
| `SetStep`, `SetStepLanes`, `AddNotes`, `RemoveNotes`, `MoveNotes`, `ResizeNotes`, `SetNoteVelocity`, `SetNoteRepeat`, `SetPatternLength`, `SetStepTicks`, `SetSwing`, `RenamePattern` | every clip playing that content (`Focus::Pattern`), and the clip editor if it shows it |
| `AddTrack`, `RenameTrack`, `SetTrackMix`, `SetSend`, `RemoveSend` | that mixer strip (`Focus::Track`) |
| `AddInsert`, `AddBuiltinInsert`, `SetFxParam`, `SetSaturatorCurve`, `SetDelayPingPong`, `SetSidechain`, `MoveInsert`, `SetPluginParam` | that insert (`Focus::Insert`) |
| `SetTempo`, `SetTimeSigNum`, `SetMetronome`, `SetLoopRegion` | the transport bar |

## Activity groups (Amendment 18)

- `SetActivity { text: Some(..) }` opens a group: remember the commit
  before it (`editor.head_name()`). Every agent commit until the activity
  ends or is replaced belongs to the group; the History page shows the
  group glowing "in progress"; the glowing objects are those the group's
  commits touched.
- An agent that never sets an activity still gets a group per request
  (the glow follows its last batch, 18.2).
- Escape (or the pill's Stop): `ControlServer::set_agents_enabled(false)`
  first (rejects in-flight and further requests, disconnects the agent),
  drop held tickets, clear the glow, and show the toast "Agent stopped"
  with "Undo Its Changes", which runs `Session::restore_version(Author::User,
  <commit before the group>)` as one undoable step.

## Replies control relies on

- `Applied` after `Undo`, `Redo`, `BranchSwitch` and `VersionRestore`
  carries the new revision (control falls back to a `ProjectGet` when it
  gets `Done`, so `Done` still works).
- `Branches` after `BranchCreate` (control falls back to `BranchList`).
- `HistoryTree.head` and `BranchInfo.head`/`base` are names that
  `resolve` accepts back (full hash, unique 8+ prefix, or the 16-hex
  provisional id). Control shows the first 16 characters.

## Protocol changes proposed (not in protocol v3 yet)

See the mcp-v3 report: `next_id` in the `Project` reply, an `Audition`
request, a `Seek` request, and a `SoundAdd` request. The bridge needs no
change until the orchestrator adds them.

## AudioClipAdd (protocol v4)

`AudioClipAdd { sound, instrument, start }` is a drop (SPEC 21.1, Amendment
31). `sound` is a catalogue id from `SoundSearch` or the hash of a sample
already in the project. The bridge imports it if needed, measures it off the
GTK thread, and applies one undo group by the request's author: `AddSample`,
then (without `instrument`) `AddTrack` and `AddChannel` with
`NewInstrument::Audio` named after the file, then `AddAudioClip` at `start`
ticks, `offset` 0, and `len` the whole sound at the project's tempo. It needs
`base_revision`; the reply is `Applied`, the clip's id last in `created`
(the row before it when the request made one).
