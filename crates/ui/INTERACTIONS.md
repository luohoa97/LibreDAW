<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Interaction contract

What every control does for each input. Binding for `crates/ui`; the
plain-struct parts have unit tests (named in the last column). When a row
says "nothing", the input is deliberately not used and passes on to the
window (for example to an accelerator or to Space for Play).

General rules (tested in `keys.rs`, `context_menu.rs`):

- One menu per object. Right click, a long press (touch), the Menu key and
  Shift+F10 open the same menu (`menus.rs`), at the pointer or under the
  focused object. Every item names an action that exists and does what the
  label says (`menus::tests`).
- Custom widgets handle only the key combinations listed here. Any other
  modifier combination falls through to the window, so Ctrl+Return,
  Alt+Left, Ctrl+Z and the rest always work (`keys::tests`).
- Nothing renames on a click. Names change with F2 or the object's Rename
  item; Return commits, Escape cancels, leaving the field commits
  (`rename_label::tests`).
- Escape closes the innermost thing first: an open menu or popover, then a
  text field being edited, then the selection, then the page (Notes back
  to Steps). It never discards an edit without Undo.
- Space is Play/Stop everywhere except in widgets that use Space
  themselves (buttons, toggles, entries, list rows).
- Every destructive action has Undo in its toast; no confirmation dialogs.

## Channel rows (Steps, header column)

| Input | Effect |
|---|---|
| Single click | Selects the channel (everywhere) and plays its sound |
| Double click | Opens its notes (Edit Notes) |
| Right click, long press, Menu, Shift+F10 | Channel menu: Edit Sound, Edit Notes, Choke Group, Rename, Remove Channel |
| Drag | Nothing yet (reorder comes with the timeline rows) |
| Return | Opens its notes |
| Space | Nothing (list row) |
| F2 | Renames the selected channel in place |
| Delete | Nothing (Remove Channel is in the menu, with Undo) |
| Escape | Clears the channel selection (stays clear until a channel is picked) |
| Tab | Next control (mute, Edit Notes button), then the grid |
| Mute button | Toggles mute; the row dims (state not by color alone) |
| Edit Notes button (wide windows) | Opens its notes |

## Step grid

| Input | Effect |
|---|---|
| Click a cell | Toggles the step; the first cell of a drag sets on or off and dragging paints it (one undo step); also selects the channel |
| Double click a cell | Toggles once (the second press is ignored: `keys::RepeatFilter`) |
| Click beside the cells | Clears the channel selection |
| Right click | Nothing yet |
| Arrows | Move the cursor cell (selects that row's channel) |
| Home, End | First, last step of the row |
| Page Up, Page Down | One bar back, forward |
| Return | Toggles the cursor step (plain Return only) |
| Delete, BackSpace | Clears the cursor step |
| Escape | Clears the channel selection |
| Space | Nothing (Play/Stop) |
| Tab | Leaves the grid (one tab stop) |
| Any modifier | Nothing: goes to the window (Ctrl+Return opens notes) |

## Piano roll (Notes)

| Input | Effect |
|---|---|
| Click empty grid | Adds a note one snap long (and plays it); dragging sets its length |
| Click a note | Selects it; Shift+click adds to the selection |
| Drag a note | Moves it (one undo step); its right edge resizes |
| Double click a note | Deletes it |
| Shift+drag on empty space | Rubber-band selection |
| Right click on a note, Menu, Shift+F10 | Note menu: Delete, Duplicate |
| Scroll / Shift+scroll | Vertical / horizontal scroll |
| Ctrl+scroll / Ctrl+Shift+scroll | Zoom time / rows |
| Arrows | Move the cursor (time by snap, rows by semitone) |
| Ctrl+arrows | Move the selected notes (Ctrl+Shift+Up/Down: an octave) |
| Shift+Left/Right | Resize the selected notes |
| Return | Adds a note at the cursor, or removes the one there |
| Delete, BackSpace | Deletes the selected notes |
| + / - | Velocity of the selected notes +- 8 |
| Ctrl+A / Shift+Ctrl+A | Select all / none |
| Escape | Clears the note selection; with none, back to Steps |
| Alt+Left, back button | Back to Steps |
| Space | Nothing (Play/Stop) |

## Mixer strip

| Input | Effect |
|---|---|
| Click name | Nothing (the strip's controls take clicks) |
| Right click, long press, Menu, Shift+F10 | Strip menu: Rename, Reset Fader, Remove Track (not on Master) |
| F2 (focus in the strip) | Renames the track; on the Mixer page F2 renames the selected track |
| Fader: drag, arrows, Page Up/Down | Volume; one undo step per gesture |
| Fader: double click, Home | 0 dB |
| Fader: End | Silence |
| Fader: scroll | Only while it has focus (scrolling the page never moves it) |
| Pan: drag, arrows; double click | Pan; centre |
| M, S (focus in the strip, no modifiers) | Mute, Solo |
| Meter: click | Resets the clip lamp |

## Sound browser

| Input | Effect |
|---|---|
| Click or Return on a built-in sound | Adds it to the project as a new channel (toast with Undo) |
| + button | Same |
| Right click, Menu, Shift+F10 | Add to Project, Replace the Selected Channel's Sound |
| Kind dropdown | Shows one kind of sound |
| Search | Filters by name, kind, pack |
| Preview | Hidden until the engine can audition (SPEC 20.3) |

## Window, header and panes

| Input | Effect |
|---|---|
| F9 / Shift+F9 | Show or hide Sounds / Inspector |
| Pane toggles | Same; the open or closed choice survives resizes and maximize |
| Narrow windows (below 1100 sp, inspector below 1400 sp) | Panes overlay the content instead of sitting beside it; opening one closes the other |
| Ctrl+1, Ctrl+2 | Pattern, Mixer |
| Ctrl+Z, Shift+Ctrl+Z | Undo, Redo (also the header buttons) |
| F10 | Main menu |
| Audio banner Retry | Tries to start audio again; the banner stays until it works |
| Launch | Keyboard focus starts in the step grid |
| `libredaw Project.ldaw` | Opens that project instead of the last one |
