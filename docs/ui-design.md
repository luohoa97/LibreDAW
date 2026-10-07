<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# LibreDAW UI design

Status: draft for owner review. Scope: GTK4 + libadwaita (gtk4-rs 0.11, libadwaita-rs 0.9, feature `v1_5`). Inputs: SPEC.md sections 11, 13, 15, 16, 17, the existing `crates/ui` view math (`view_math.rs`, `roll_logic.rs`), and the GNOME HIG pages cited inline.

HIG pages fetched and read for this document: header bars, view switchers, utility panes, sidebars (nav), banners, toasts, dialogs, placeholders, menus, sliders, adaptive layout, keyboard guideline, standard shortcut table, accessibility, writing style, UI styling. Not reachable at the time of writing (HTTP 404): pointer and touch, tooltips, toolbars, containers/sidebars. Rules that would come from those pages are marked "our rule" and are not attributed to the HIG.

## 0. Decisions that need an owner or lead answer

These conflict with SPEC text or with the brief. The rest of the document assumes the recommended answer.

1. Transport placement. SPEC 11 says "header bar transport controls". The HIG says header bars hold "a small number of controls" and must leave drag space ([Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)). A centered view switcher plus play, stop, metronome, position, and tempo does not fit. Recommended: a transport bar directly under the header bar (a second top bar in the same `AdwToolbarView`). Amend SPEC 11 to "transport bar under the header bar".
2. Space. SPEC 11 says Space toggles a step or note in the grids. Every DAW user expects Space to be play/stop, and the brief says so. Recommended: Space is play/stop everywhere except in widgets that consume Space themselves (buttons, switches, entries, list rows, the sound browser). In the step grid and piano roll, Return toggles a step or adds/removes a note. Amend SPEC 11.
3. Capitalization. The brief says sentence case. The HIG says header capitalization for "button labels, switch labels, menu items and tooltips" and for view titles, and sentence capitalization for labels of check boxes, radio buttons, sliders, text entries, field labels, combobox labels, and body text ([Writing Style](https://developer.gnome.org/hig/guidelines/writing-style.html)). This document follows the HIG (section 6).
4. "Advanced" versus "Expert". SPEC 15.10 calls the plugin-window button "Advanced"; the brief says "Expert". This document uses "Expert Window".
5. View names. SPEC uses "Playlist". That word is a proprietary product's term for the same panel, and "Channel Rack" and "Piano Roll" are too (the last is also generic). We use our own words in the UI: Pattern, Song, Mixer, with "Steps" and "Notes" as the two sections of the Pattern view. SPEC and code can keep `playlist` internally.
6. Three switcher pages, not four. "Channels/Steps" and "Piano Roll" are both parts of editing one pattern and are used together, so they share one page (a vertical split). The view switcher then has three pages (Pattern, Song, Mixer), which is the HIG minimum of "between three and five views" ([View Switchers](https://developer.gnome.org/hig/patterns/nav/view-switchers.html)).
7. Libadwaita 1.5 limits. Not available, do not use: `AdwBottomSheet`, `AdwMultiLayoutView`, `AdwSpinner`, `AdwButtonRow` (1.6); `AdwToggleGroup`, `AdwWrapBox`, `AdwInlineViewSwitcher` (1.7); `AdwShortcutsDialog` (1.8); the system accent color API (1.6). Available: `AdwToolbarView`, `AdwBreakpoint`, `AdwOverlaySplitView`, `AdwNavigationSplitView`, `AdwNavigationView`, `AdwBanner`, `AdwDialog`, `AdwAlertDialog`, `AdwPreferencesDialog`, `AdwAboutDialog`, `AdwSwitchRow`, `AdwSpinRow`, `AdwEntryRow`, `AdwComboRow`, `AdwExpanderRow`, `AdwViewStack`, `AdwViewSwitcher`, `AdwViewSwitcherBar`, `AdwToastOverlay`, `AdwStatusPage`, `AdwClamp`, `AdwWindowTitle`, `AdwTabView`/`AdwTabBar`. The keyboard-shortcuts window is the GTK `GtkShortcutsWindow` (deprecated upstream, still works).
8. Open file dialog. "Open Project..." and "Add Sound Folder..." use `GtkFileDialog`, a system modal that appears only in response to a deliberate click. SPEC 17.1 lists modal exceptions for agent approvals only; this document treats user-initiated file choosers as outside the "no modal during editing" rule (dialogs are acceptable "in immediate response to a deliberate user action", [Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)). Confirm.

## 1. Principles

Each rule names the HIG page it comes from. Where a rule is ours, it says so.

1. Use libadwaita widgets first; draw only what has no widget. Header bar, view switcher, boxed lists, status pages, toasts, banners, preferences dialog, and menus are all stock. Only the step grid, piano roll, playlist canvas, meters, knobs, and mini clip previews are custom. Reason: custom styling is "minimum custom styling to reduce maintenance and bugs", and style classes and named colors "automatically adjust across light, dark, and high-contrast modes" ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html)).
2. Keep the header bar sparse and put the view switcher in its center. Left: actions on content (sidebar toggle, undo, redo). Center: the view switcher. Right: the inspector toggle and the primary menu. Always leave blank space to drag the window ([Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)). Header controls are icon buttons, flat, with tooltips.
3. Sidebars are utility panes that overlap, never squeeze. The sound browser (left) and the inspector (right) are `AdwOverlaySplitView`s: persistent on wide windows, overlaying the content when narrow ([Utility Panes](https://developer.gnome.org/hig/patterns/containers/utility-panes.html), [Adaptive Layout](https://developer.gnome.org/hig/guidelines/adaptive.html)). Toggle with F9 ([Standard Keyboard Shortcuts](https://developer.gnome.org/hig/reference/keyboard.html)).
4. Design from 360 px upward. The window works at 360x294 ([Adaptive Layout](https://developer.gnome.org/hig/guidelines/adaptive.html)). Wide screens add visible panes; they never add features. Avoid cutting the window into "numerous small panes", which "resist adaptive scaling" (same page). Hence at most two sidebars plus one content area, with one vertical split inside the Pattern page.
5. No modal interruption while editing; undo instead of confirmation. "Undo is typically a better option than a confirmation dialog"; dialogs "should only ever be displayed in immediate response to a deliberate user action" ([Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)). Errors and confirmations are toasts ([Toasts](https://developer.gnome.org/hig/patterns/feedback/toasts.html)); ongoing states are banners ([Banners](https://developer.gnome.org/hig/patterns/feedback/banners.html)). This matches SPEC 15.8.6 and the "close saves" rule in SPEC 11.
6. Everything works with a keyboard and a screen reader. Every part of the app is reachable by keyboard; every element has "a short and descriptive" accessible name; the high-contrast style is tested ([Accessibility](https://developer.gnome.org/hig/guidelines/accessibility.html)). Custom widgets set an accessible role and update label and value as the cursor moves (SPEC 11).
7. Never carry meaning in color alone. The HIG asks that styling not rely "solely on color" ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html)). On steps, notes, mute, solo, clipping, and approvals, a shape, a position, a label, or a height carries the same information. Channel colors are identity hints only.
8. Empty states teach the next action. Every empty area is an `AdwStatusPage` with a heading, one description line, and (if useful) a suggested-style button ([Placeholders](https://developer.gnome.org/hig/patterns/feedback/placeholders.html)). A beginner never faces a blank grid.
9. Plain words on controls, standard words in menus. Buttons use specific verbs, never "OK" ([Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)); menu items are verbs for commands and adjectives for settings, in header capitalization ([Menus](https://developer.gnome.org/hig/patterns/controls/menus.html)). Jargon (quantize, ratchet) is allowed only where a plain word would lose precision, and it gets a tooltip.
10. Follow the standard shortcut table and add ours only where it has no meaning ([Keyboard](https://developer.gnome.org/hig/guidelines/keyboard.html), [Standard Keyboard Shortcuts](https://developer.gnome.org/hig/reference/keyboard.html)). Ctrl+letter for common commands; Shift+Ctrl+letter for reverse or extend (redo, deselect); avoid Alt (access keys) and Super (system).

What we borrow from GNOME apps with dense editing UIs (in structure, not in appearance): the stock "header bar plus view switcher in the center, switcher bar at the bottom when narrow" layout used by libadwaita's own demo and most adaptive GNOME apps; the "toolbar below the header bar" row that GNOME text and image editors use for secondary tools, here used for transport; the "overlay split view" side pane that GNOME file and chat apps use, here used for sounds and the inspector; the status-page empty state pattern; and the preferences-dialog pattern for settings. We do not borrow any DAW's layout names or look: our pattern editor is one vertical split (steps over notes), not a channel-rack window plus floating piano-roll window; our song view is called Song; our strips are our own proportions.

## 2. Window structure

### 2.1 Widget tree

```
GtkApplication "org.libredaw.LibreDAW" (app id to be confirmed in ASSETS/packaging)
AdwApplicationWindow  (default 1360x800, width-request 360, height-request 294)
  breakpoints: bp-regular, bp-compact, bp-narrow, bp-short   (section 2.4)
  content:
  AdwToastOverlay                                  <- all toasts (errors, "Saved to Music/...")
    AdwNavigationView  "root"                      <- start screen / project
      AdwNavigationPage "start"  (title "LibreDAW")
        AdwToolbarView
          top:  AdwHeaderBar (title: AdwWindowTitle "LibreDAW"; end: primary menu)
          content: StartScreen   (3.1)
      AdwNavigationPage "project"  (title = project name)
        AdwToolbarView  "project_toolbar"
          top bar 1: AdwHeaderBar  "main_header"
              start: [sounds toggle] [undo] [redo]
              title-widget: GtkBox  "title_box"
                  AdwViewSwitcher "switcher" (policy wide, stack = workspace)
                  AdwWindowTitle  "narrow_title" (visible only in bp-narrow)
              end:   [agent status button, only while an agent session is on]
                     [inspector toggle] [primary menu button]
          top bar 2: GtkBox.toolbar "transport_bar"                   (section 5)
          top bar 3: AdwBanner "agent_banner" (revealed on pending approvals, 3.9)
          content: AdwOverlaySplitView "browser_split"   (sidebar start, 300 px)
              sidebar: SoundBrowser                                    (3.8)
              content: AdwOverlaySplitView "inspector_split" (sidebar end, 340 px)
                  sidebar: Inspector  (AdwToolbarView)
                      bottom bar: AdwViewSwitcherBar "inspector_switcher" (reveal=true)
                      content: AdwViewStack "inspector_stack"
                           page "sound"   (instrument panel, 3.7)
                           page "agent"   (activity panel, 3.9)
                           page "history" (versions, 3.10)
                  content: AdwToolbarView
                      content: AdwViewStack "workspace"
                           page "pattern"  title "Pattern"   icon  libredaw-pattern-symbolic
                           page "song"     title "Song"      icon  libredaw-song-symbolic
                           page "mixer"    title "Mixer"     icon  libredaw-mixer-symbolic
                      bottom bar: AdwViewSwitcherBar "switcher_bar" (revealed in bp-narrow)
```

Notes on the tree:

- `AdwApplicationWindow` accepts breakpoints directly (`add_breakpoint`). Because breakpoint setters can only set properties on widgets inside the window, every widget a breakpoint touches has a builder id.
- `AdwToolbarView` is what draws the flat, merged header bar and top/bottom bars; do not hand-build a `GtkBox` with a header bar.
- The start screen and the project are two pages of one `AdwNavigationView`. "New Project" (Ctrl+N) saves the current project (close saves, SPEC 11), pushes "start", and the back gesture or Esc returns to the open project. No dialog is involved. The start page never shows a back button when no project is open.
- The window title (for the shell and Alt+Tab) is "<project name> - LibreDAW". It is not shown inside the header bar on wide windows because the view switcher occupies the title area; on narrow windows `narrow_title` shows it.
- Icons for the three views are our own symbolic SVGs (GPL, SPDX comment, listed in `ASSETS.md`). Until they exist, use the stand-ins `view-list-symbolic` (Pattern), `view-continuous-symbolic` (Song), and `audio-volume-high-symbolic` (Mixer).

### 2.2 Header bar contents

| Slot | Widget | Icon (stock until ours exist) | Tooltip | Action |
|---|---|---|---|---|
| start | `GtkToggleButton.flat` "sounds toggle" | `sidebar-show-symbolic` | "Show Sounds (F9)" / "Hide Sounds (F9)" | toggles `browser_split.show-sidebar` |
| start | `GtkButton.flat` | `edit-undo-symbolic` | "Undo (Ctrl+Z)" | `win.undo`, insensitive when nothing to undo |
| start | `GtkButton.flat` | `edit-redo-symbolic` | "Redo (Shift+Ctrl+Z)" | `win.redo` |
| center | `AdwViewSwitcher` | - | - | pages Pattern, Song, Mixer |
| end | `GtkButton.flat` (only while agent session on) | `libredaw-agent-symbolic` (stock stand-in: `network-workgroup-symbolic`) | "An agent is connected" | opens inspector, "agent" page |
| end | `GtkToggleButton.flat` | `sidebar-show-right-symbolic` | "Show Inspector (Shift+F9)" | toggles `inspector_split.show-sidebar` |
| end | `GtkMenuButton.flat` | `open-menu-symbolic` | "Main Menu" | primary menu |

Why: left holds actions on content, center the switcher, right the menu ([Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)). Buttons are borderless; no label-only buttons, no suggested or destructive styles, no linked groups in the header bar (same page). The undo/redo pair is two separate flat buttons, not a linked pair. The primary menu uses `open-menu-symbolic` and the label "Main Menu" ([Menus](https://developer.gnome.org/hig/patterns/controls/menus.html)). Every header button has a tooltip.

Controls that stay out of the header bar: play, stop, tempo, metronome (transport bar), export (menu), search (sound browser has its own).

### 2.3 Primary menu

Ten items, three sections, no Quit or Close (the HIG says omit them; the window close button and Ctrl+Q cover it) ([Menus](https://developer.gnome.org/hig/patterns/controls/menus.html)). Header capitalization, verbs, access keys on every item.

```
Section 1 (project)
  New Project            Ctrl+N          win.new
  Open Project...        Ctrl+O          win.open
  Save                   Ctrl+S          win.save           (explicit save; close and autosave also save)
  Save As...             Shift+Ctrl+S    win.save-as
Section 2 (output)
  Export Audio...        Ctrl+E          win.export         (opens a non-modal export panel in the Song view, see 3.6; "..." because it needs input)
  Project Properties     Alt+Return      win.properties     (name, tempo default, key, time signature)
Section 3 (standard end group, in this order)
  Preferences            Ctrl+,          app.preferences    (AdwPreferencesDialog)
  Keyboard Shortcuts     Ctrl+?          win.show-help-overlay
  Help                   F1              app.help
  About LibreDAW                         app.about          (AdwAboutDialog)
```

"Export Audio" opens a popover-style panel, not a dialog, anchored in the Song view toolbar (3.6). If implementation cost forces a dialog, use `AdwDialog` with a verb button "Export", parented to the window. The menu has no "Undo History" item: History is a page in the inspector (3.10).

### 2.4 View switching: why a switcher, not tabs or panes

- Pattern, Song, and Mixer are three parallel top-level views of the same project, each needing most of the screen. That is the textbook view switcher case ([View Switchers](https://developer.gnome.org/hig/patterns/nav/view-switchers.html): three to five views, noun labels in header capitalization, roughly equal length). A sidebar of views is for "a larger number of views than can be accommodated in a standard view switcher" ([Sidebars](https://developer.gnome.org/hig/patterns/nav/sidebars.html)); we have three. Tabs (`AdwTabView`) are for documents, not views.
- The sound browser and inspector are not views, they are utility panes that affect or support the main view ("place the pane on the left if it affects the main view, or on the right if it is secondary", [Utility Panes](https://developer.gnome.org/hig/patterns/containers/utility-panes.html)). Browser on the left (it creates content), inspector on the right (it edits the selection).
- Inside the Pattern page, steps and notes are one vertical `GtkPaned` (not a view switcher) because a beat maker alternates between them constantly and both depend on the selected channel. Double-clicking the divider toggles which half is maximized.
- Switcher policy: `AdwViewSwitcher` with `policy=wide` (icon beside label) in the header. In `bp-narrow` the switcher in the header is hidden and `AdwViewSwitcherBar` at the bottom is revealed ([View Switchers](https://developer.gnome.org/hig/patterns/nav/view-switchers.html): switchers move to the bottom edge when space is constrained).
- Do not use `AdwViewSwitcherTitle`; it is deprecated since 1.4. Put the switcher into a `title_box` and flip visibility from the breakpoint.
- Activity badge: the Song and Mixer pages set `needs-attention` on their switcher button when the agent changed them while another view was in front (see 3.9). Cleared on visit.

### 2.5 Size classes and breakpoints

Breakpoint lengths use `sp` so they scale with the user's text size setting. Widths are logical pixels (scale factor 1).

| Name | Condition | What changes |
|---|---|---|
| default (wide) | width >= 1400sp | Browser sidebar and inspector sidebar can both be shown side by side (`collapsed=false`). Transport bar shows everything. |
| `bp-regular` | `max-width: 1399sp` | Both split views `collapsed=true`: sidebars overlay the content instead of sitting beside it, and `show-sidebar` defaults to false. Opening one closes the other (code). |
| `bp-compact` | `max-width: 900sp` | Transport bar collapses key, time signature, and Pattern/Song mode into a "Transport Settings" popover button. Step name column 140 px instead of 200 px. Mixer strips 80 px instead of 96 px. Piano roll keyboard column stays 56 px (existing `key_w`). |
| `bp-narrow` | `max-width: 600sp` | `switcher` hidden, `narrow_title` shown, `switcher_bar.reveal=true`. Header: undo and redo stay at start; inspector toggle hidden (the inspector opens from a channel's "Sound" button); end keeps sounds toggle and menu. Transport bar shows only skip-back, play/stop, position, tempo button. Touch sizes on (row height 44 px, handles 24 px). Pattern view shows one section at a time. Browser and inspector overlays are full width (`max-sidebar-width` ignored by `collapsed`). |
| `bp-short` | `max-height: 760px` | Pattern view shows steps and notes in the same paned but defaults to a 40/60 split and the lane editor is limited to 1 lane; piano roll `row_h` clamps to 16. Mixer meters shrink to 120 px. |
| `bp-narrow` and short | `max-width: 600sp and max-height: 500px` | Landscape phone: transport bar becomes single row; no switcher bar (switcher in header). |

Breakpoint setters (declarative where possible):

```
bp-regular:  browser_split.collapsed = true ; inspector_split.collapsed = true
bp-compact:  transport_extras.visible = false ; transport_menu.visible = true
bp-narrow:   switcher.visible = false ; narrow_title.visible = true ; switcher_bar.reveal = true ;
             inspector_toggle.visible = false ; touch.css-class = "touch"   (via signal, code)
```

Class names that cannot be set declaratively (like adding "touch" to the root) are applied in the `apply` and `unapply` signal handlers. Size class logic itself (which section is maximized, which strips are narrow) lives in a plain `SizeClass` struct with unit tests, as SPEC 11 asks for thin widget wrappers.

Sidebar widths: `browser_split` 300 px (min 260sp, max 360sp, fraction 0.2), `inspector_split` 340 px (min 300sp, max 420sp). These are `min-sidebar-width`, `max-sidebar-width`, `sidebar-width-fraction` of `AdwOverlaySplitView`.

### 2.6 What is visible at once

At 1920x1080 (wide):
- Header bar (47 px), transport bar (44 px), sound browser (300 px, shown), inspector (340 px, shown), workspace (1280 px by about 960 px).
- Pattern page: steps section (top, about 8 channel rows plus one lane editor, roughly 420 px) and notes section (bottom, about 480 px: ruler, 20 rows at 20 px, velocity lane). Everything in one glance, no scrolling for a typical 8-channel beat.
- Song page: about 12 tracks by 32 bars.
- Mixer page: master plus about 11 strips at 96 px, plus returns.

At 1280x720 (`bp-regular`, `bp-short`):
- Header, transport bar, and the workspace at the full 1280 px. Browser and inspector are overlays opened with F9 and Shift+F9 (or toggle buttons). Pattern: steps top (about 5 rows) and notes bottom (about 13 piano rows plus velocity lane) in the 40/60 split; the user can drag the divider or double-click it to maximize either half. Mixer: about 12 strips.

At 360x640 portrait (`bp-narrow`, also `bp-compact`, `bp-regular`):
- Header (undo, redo, project name, sounds, menu), transport bar (skip back, play/stop, position, tempo), one workspace view at a time, switcher bar at the bottom. Pattern shows steps only until a channel's "Edit Notes" button is pressed, then notes fill the page with a back button "Steps" at the start of the header bar (replacing undo and redo while in this sub-state; undo and redo move into the transport bar's popover). 8 visible steps per screen with horizontal scroll; step cells 36x44.

The sub-state ("steps only", "notes only", "both") is one enum, `PatternFocus`, stored in the view state file (SPEC 11, `.view.toml`) and chosen automatically by size class unless the user overrides it with the divider.

### 2.7 Window behaviors

- Close saves (SPEC 11, Amendment 9): no confirmation, the window waits for the save and shows the error in a toast if it fails (window stays open).
- Window geometry, selected view, sidebar states, pattern focus, zoom, and scroll are stored in `.view.toml` (SPEC 11). Use `GtkApplicationWindow` default-size/maximized properties through `GSettings` or the same file; one source only (the view file).
- Color scheme: follow the system by default; the preference offers "Follow System", "Light", "Dark" ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html): per-app preferences "light, dark, and follow system"). The default is Follow System. A DAW is content-rich and many users prefer dark; the HIG allows dark as a recommendation for content-rich apps but says most apps default to light/system, so we do not force it.

## 3. Views in detail

Conventions used below:

- "Stock" means a libadwaita or GTK widget with no custom drawing. "Custom" means a `GtkWidget` subclass drawing with `snapshot()`, with logic in plain structs (SPEC 11).
- Style classes named `ldaw-*` are ours and defined in `data/style.css`. All others are libadwaita or GTK classes. `.dim-label` is the 1.5 class for secondary text (it is renamed `.dimmed` in libadwaita 1.6; switch when the minimum version rises).
- Accessible names are plain sentences, unique per element ("Step 5 of Kick", not "step").
- Mouse and keyboard are listed together; every mouse edit has a keyboard equivalent (SPEC 11). Pointer rules marked "our rule" come from us, not the HIG.

### 3.1 Start screen (SPEC 15.7)

Purpose: from launch to hearing a beat in two clicks (SPEC 15.8.1): click 1 on a template card, click 2 on Play.

Shown when there is no last project, when the user chooses New Project, or when opening the app with `--start`. On normal launch the last project reopens with its view (SPEC 11, Amendment 9).

Layout (`AdwNavigationPage "start"`):

```
AdwToolbarView
  top: AdwHeaderBar  (title: AdwWindowTitle "LibreDAW", end: primary menu; start: back button if a project is open)
  content: GtkScrolledWindow
    AdwClamp (maximum-size 960, tightening-threshold 640)
      GtkBox vertical, spacing 24, margin 24
        GtkLabel.title-1 "What do you want to make?"       <- sentence-form body text, sentence case
        GtkLabel.body.dim-label "Pick a style and press Play. You can change everything later."
        GtkLabel.heading "Start a Beat"        (section heading, header caps)
        GtkFlowBox "templates" (min 2, max 5 children per line, homogeneous, selection none, activate on single click)
          TemplateCard x N   (Phonk, Trap, Boom Bap, Lo-fi, House, Empty Project)
        GtkLabel.heading "Recent Projects"
        GtkListBox.boxed-list "recent"   (AdwActionRow each)  | or empty state
        GtkLabel.heading "Demo Projects"
        GtkListBox.boxed-list "demos"    (AdwActionRow each)
        GtkBox horizontal: [Open Project...] button (flat, pill not needed)
```

TemplateCard (custom composite, stock parts): a `GtkButton.card.ldaw-template-card` (a focusable activatable card) containing a vertical box: a 64 px symbolic template icon (our SVG, one per genre; stand-in `audio-x-generic-symbolic`), `GtkLabel.heading` genre name, `GtkLabel.caption.dim-label` "140 BPM - 8 channels". Size 160x144, `min-width` so five fit in 960. Activate: creates the project from the template (never asks technical questions, SPEC 15.7), pushes the "project" page, selects the Pattern view, and moves keyboard focus to the Play button.

- Click 2 is the Play button, so the transport bar must be visible and focused after click 1. While the project has never been played, the Play button carries `.suggested-action` (our rule: the one suggested action in the window, [UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html) wants suggested used sparingly); after first play it is a normal flat toggle.
- Optional shortcut to hearing in one click: a small flat circular play button in each card's top corner (`media-playback-start-symbolic`, tooltip "Preview Beat") that auditions the template's pattern without opening the project (uses the preview slot, SPEC 17.2 ux-5). Not required for the 2-click rule.
- Recent project row: `AdwActionRow`, title = project name, subtitle = "Edited 2 hours ago - 140 BPM" (`.numeric` not needed), prefix = our project icon, suffix = `go-next-symbolic`. Activate opens it. Context menu (right-click or Menu key): "Show in Files", "Remove from List". No delete here (project deletion is PRIVILEGED for agents and gets an `AdwAlertDialog` for humans, not on this screen).
- Demo row: same, subtitle = "Demo - 128 BPM - by LibreDAW". Opens a copy under Music/LibreDAW (never edits the shipped file).
- Open Project...: `GtkButton.flat` with label, opens `GtkFileDialog` (decision 8).

Empty state, recent list: no row list; instead an `AdwStatusPage.compact` with icon `document-open-recent-symbolic`, title "No Recent Projects", description "Projects you open appear here." No button (the cards above are the action).

Empty state, no templates found (broken install): `AdwStatusPage` icon `dialog-warning-symbolic`, title "Templates Missing", description "Reinstall LibreDAW or open a project from your files." Button "Open Project..." (suggested, pill).

Keyboard: Tab order is cards (arrow keys move within the flow box), recent list, demo list, Open Project. Enter activates. Ctrl+O opens the file chooser. Ctrl+N is a no-op here.

Accessible: the flow box is a list; each card has the label "Trap template, 140 BPM, 8 channels". Focus ring is the stock `.card` focus ring.

### 3.2 Pattern view: overview

**Amendment (owner, 2026-10-07): replaces the vertical split below.** The Pattern page is an `AdwNavigationView`: the root page "Steps" (pattern toolbar plus channel list with steps), and a pushed `AdwNavigationPage` per channel titled with the channel name that shows the piano roll, with the standard back button, swipe, Esc, and Alt+Left. "Edit Notes", double-click on a channel name, and Return on a channel push it. There is no `GtkPaned`, no `PatternFocus` button, and no hand-drawn separator anywhere in the app: structure comes from `AdwToolbarView` top bars, the `.view` background, spacing, and stock selection styles, as in GNOME core apps. The text below about the split is kept for history only.

`AdwViewStack` page "pattern". Content is a `GtkPaned` (vertical, `wide-handle=true`, `shrink-start-child=false`, `shrink-end-child=false`, `resize-start-child=true`, `resize-end-child=true`, `position-set` stored in the view file):

```
GtkPaned vertical
  start child:  StepsSection   (3.3)
  end child:    NotesSection   (3.4)
```

Divider: double-click toggles "maximize steps" and "maximize notes" by hiding the other child (the paned then shows one child fully); Esc or double-click again restores. The state is the `PatternFocus` enum (both, steps, notes).

Selection model: one selected channel. Selecting a channel (click its name in Steps, or its tab in Notes) changes the Notes section to that channel, the Inspector "sound" page to that channel, and the playlist brush pattern is unrelated (the pattern selector is global to the Pattern view and the Song brush).

Pattern selector (in a `GtkBox.toolbar` strip at the top of the Pattern view, above the paned, one row):

- `GtkMenuButton` "Pattern 1" (label updates, icon `pan-down-symbolic`) opening a popover with a `GtkListView` of patterns (rename inline with F2 or double-click, color dot, "New Pattern" at the end as a flat row). Tooltip "Choose Pattern".
- `GtkButton.flat` `list-add-symbolic` tooltip "New Pattern" (duplicate of the popover entry, one click).
- `GtkSpinButton` "Length" in bars (1 to 8, label "Bars" after it in `.dim-label`), tooltip "Pattern Length in Bars".
- `GtkScale` "Swing" horizontal 0 to 75, draw-value on, value format "%d%%", marks at 0 and 50 ([Sliders](https://developer.gnome.org/hig/patterns/controls/sliders.html): real-time feedback, marks, value text). Width 140 px. Accessible name "Swing". Moves the grid via the shared pure swing function (SPEC 17.2 rt-3), so the grid shows swung step offsets live (offset columns by half a cell at 50 percent maximum scale; cells keep their column but their playhead flash is delayed).
- Right end, `GtkToggleButton.flat` `view-fullscreen-symbolic`-like stand-in `view-dual-symbolic`, tooltip "Focus Steps or Notes" cycling the `PatternFocus` enum. Hidden in `bp-narrow` (there the focus is controlled by "Edit Notes" and the back button).

At `bp-compact`, Length and Swing move into a `GtkMenuButton` "Pattern Settings" popover containing `AdwPreferencesGroup`-style rows (not a nested preferences page: `AdwSpinRow` and a `GtkScale` in an `AdwActionRow`).

### 3.3 Steps section (channel list with step rows)

Widget: custom `LdawStepGrid` (a single `GtkWidget` for all rows, drawing only visible rows and columns, SPEC 11) beside a stock channel header column. One custom widget for the grid, not one widget per cell: performance and focus management.

Geometry (default; touch in brackets):

| Item | Size |
|---|---|
| Channel header column | 200 px (140 at `bp-compact`, 120 at `bp-narrow`) |
| Row height | 40 px (44 touch). Row gap 2 px |
| Step cell | 28 wide x 32 high (36x44 touch), gap 2 px, radius 4 px |
| Beat group gap | extra 8 px every 4 steps; every second group shaded |
| Ruler | 24 px: bar number at bar start (`.caption`, `.numeric`), tick at each beat |
| Lane editor row | 56 px high, only under the selected channel, only when a lane is chosen |

Channel header (stock widgets in a `GtkListView`, rows aligned to the step rows by shared row height and a shared vertical `GtkAdjustment`):

```
[4px color bar] [Mute] [icon] [Name label, 1 line, ellipsized] [Sound button] [Edit Notes button]
```

- Color bar: `ldaw-channel-color` (a 4 px wide `GtkBox` with background from the channel's palette color; our rule: identity only, the row also has the name).
- Mute: `GtkToggleButton.flat.circular` `audio-volume-high-symbolic` (unmuted) and `audio-volume-muted-symbolic` (muted, checked); tooltip "Mute Channel". A muted row's cells draw at 40 percent opacity and the name gets `.dim-label` (state is visible without color).
- Icon: instrument kind (drum, synth, 808, sampler, plugin), symbolic.
- Name: `GtkLabel`; double-click or F2 edits inline via a `GtkEditableLabel`.
- Sound: `GtkButton.flat` with icon `preferences-system-symbolic`, tooltip "Edit Sound", opens the Inspector on this channel (at `bp-narrow` it is the only way to open the inspector).
- Edit Notes: `GtkButton.flat` `document-edit-symbolic`, tooltip "Edit Notes", sets focus to the Notes section for this channel (at `bp-narrow` it navigates).
- At `bp-compact` and narrower, Sound and Edit Notes fold into a single `GtkMenuButton.flat` `view-more-symbolic` ("Channel Menu") with items "Edit Sound", "Edit Notes", "Rename", "Remove Channel".
- Context menu (right-click, Menu key, Shift+F10): "Rename", "Change Color", "Duplicate" (Ctrl+U), "Clear Steps", "Remove Channel". "Remove Channel" has no confirmation; it shows a toast "Channel removed" with button "Undo".
- Drag reorder: the row handle is the icon; drag source on the header row, drop indicator line between rows. Keyboard equivalent: Alt+Up and Alt+Down move the selected channel (matches many GNOME list apps; our rule).

Step grid interactions:

| Input | Effect |
|---|---|
| Click a cell | Toggle step on/off; the first click of a drag sets the paint mode (on or off) and dragging across cells paints that mode (one undo entry per drag, SPEC 6 gesture rule) |
| Shift+click | Select a range of cells of one row (without toggling) |
| Right-click cell | Popover menu: "Clear Step", "Set Velocity", "Set Pitch", "Set Ratchet" |
| Scroll (plain) | Scroll vertically through channels |
| Shift+scroll | Scroll horizontally |
| Ctrl+scroll | None here (zoom is not meaningful for steps); do not hijack |
| Arrow keys | Move the cursor cell (SPEC 11); the cursor wraps nowhere |
| Return | Toggle the cursor step (decision 2; SPEC says Space) |
| Delete | Clear selected steps |
| Home / End | First / last step of the row |
| Page Up / Page Down | Previous / next bar |
| Ctrl+Up / Ctrl+Down | Change the velocity of the cursor step by 8 (only with the Velocity lane open); Ctrl+Shift+Up/Down for pitch lane +-1 semitone, Ctrl+Left/Right ratchet down/up (our rule; SPEC 11 keys for notes also use Ctrl+arrows) |
| Tab | Moves focus out of the grid (grid is one tab stop; arrows inside) |

Cell drawing: off = 1 px outline using `view_fg_color` at 20 percent (60 percent in high contrast), fill transparent; on = filled with the channel color, opacity = 0.45 + 0.55 x velocity/127 (visible velocity), plus a 3 px bar along the bottom edge whose width is velocity percentage when the Velocity lane is closed and the cell is wider than 20 px. Ratchet cells draw N small vertical ticks over the fill (N = repeats). Pitch-offset cells draw the signed offset ("+3") in `.caption` if cell is at least 24 px wide. Playing column: a 2 px accent outline on the current column's cells and a header triangle in the ruler; on-cells of the playing column are drawn 15 percent lighter. The cursor cell draws a 2 px inset rounded rectangle in `@accent_color` (in high contrast: `window_fg_color`) only when the grid has `focus-visible`.

Per-step lanes (SPEC 15.4):

- A lane selector sits at the grid's top-left corner above the channel header column: three `GtkToggleButton`s, linked (allowed here; it is not a header bar): "Velocity", "Pitch", "Ratchet" (only one active; clicking the active one closes the lane). Tooltips: "Show Velocity Lane", "Show Pitch Lane", "Show Ratchet Lane".
- The lane opens as a 56 px row directly under the selected channel's step row, aligned to the same columns.
- Velocity lane: a vertical bar per active step, height proportional to velocity 1-127, drag to paint (a drag across steps sets each step's bar to the pointer height, one undo group), scroll over a bar adjusts by 4, Ctrl+Up/Down by 8 from keyboard. A 1 px baseline and marks at 32, 64, 96 (faint).
- Pitch lane: bars grow up or down from a center line for -24..+24 semitones, snapped to whole semitones, label of the value in `.caption.numeric` on hover/focus and always for the cursor step. When key and scale lock is on (SPEC 15.10.5), out-of-scale values draw with a hatched outline (shape, not just color) and snap to scale degrees when Shift is not held.
- Ratchet lane: for each active step a number chip showing 1, 2, 3, 4, 6, or 8 (the allowed repeats in SPEC 15.4). Click cycles up, Shift+click cycles down, scroll changes, Return advances. Rejected values (that do not divide the step length, SPEC 17.2) are never offered.
- Lane editor accessible role: `grid` with a label "Velocity lane for Kick".

Empty state: `AdwStatusPage` replaces the whole Steps section when the pattern has no channels:

- icon `audio-x-generic-symbolic` (our `libredaw-drum-symbolic` later), title "No Channels Yet", description "Add a sound to start your beat. You can also drag one in from the sounds list."
- child: `GtkMenuButton.suggested-action.pill` "Add Channel" with a popover menu: "Drum Sound...", "Synth", "808 Bass", "Sampler" (items appear as milestones land; Milestone A shows only "Synth" and "Plugin..."). "Drum Sound..." opens the sound browser (F9) filtered to Drums.

When channels exist, a trailing row "Add Channel" (`GtkButton.flat` with `list-add-symbolic` and label) sits under the last row; same menu.

Drop targets: dropping a sound from the browser on the empty area or the "Add Channel" row creates a channel (SPEC 15.8.4); dropping on a channel header replaces its sound (undoable, toast "Sound changed - Undo"); dropping an audio file from Files imports a sample (local-only rules in SPEC 17.2 apply).

Accessibility: grid role `grid` (SPEC 11). The cursor cell updates `accessible-label` to "Step 5 of Kick, on, velocity 96" (via `update_property`, `LABEL`) and `accessible-value-text` (VALUE_TEXT) with the same string on each cursor move. Rows are announced by channel name through the header list.

### 3.4 Notes section (piano roll)

Custom widget `LdawPianoRoll` in a `GtkScrolledWindow`-like container (we own the scroll offsets in `Viewport`, see `view_math.rs`) plus stock toolbar. Geometry reuses `Viewport`: keyboard column 56 px, ruler 22 px, velocity lane 64 px (default; draggable 40 to 160 px), key row height 16 px (change default to 18; clamp 14 to 32; 16 at `bp-short`, 22 touch), time zoom `px_per_tick` 0.01 to 1.2.

Toolbar (`GtkBox.toolbar`, above the roll):

| Control | Widget | Notes |
|---|---|---|
| Channel | `GtkDropDown` "Kick" with color dot and icon | Switch which channel is edited; shows "No channel selected" if none |
| Snap | `GtkDropDown` "Snap: 1/16" | Values: "Off", "1/4", "1/8", "1/16", "1/32", "1/16 Triplet". Tooltip "Snap to Grid". Shift held during a drag bypasses snap |
| Stay in key | `GtkToggleButton` `view-grid-symbolic` + "Stay in Key" label | SPEC 15.10.5; highlights and snaps to the project key; tooltip "Highlight and snap to the project key" |
| Zoom out / in | two `GtkButton.flat` `zoom-out-symbolic`, `zoom-in-symbolic` | Ctrl+- / Ctrl++ ; Ctrl+0 resets ([Standard Keyboard Shortcuts](https://developer.gnome.org/hig/reference/keyboard.html)) |
| Velocity lane | `GtkToggleButton.flat` | Show or hide the velocity lane; tooltip "Show Velocity Lane" |

Keyboard column:

- 56 px wide. White keys full width, black keys 60 percent of the width and drawn darker, aligned to the grid rows. Every C key shows its label ("C3") at the right of the key in `.caption.numeric`, using scientific naming with middle C = C4 (key 60), exactly as `note_name()` in `view_math.rs`. Other keys show no label until hovered or focused (then their name appears).
- Click or drag across keys auditions the pitch through the preview slot (SPEC 17.2 ux-5), no note is added.
- Root note of the project key gets a small accent dot at the left edge.

Ruler: bar numbers at bar lines (`.caption.numeric`), beat ticks, sub-beat ticks only when zoomed in (the `grid_lines` min-pixel rule). A loop region strip (6 px tall) sits at the bottom of the ruler; drag to define a loop; click clears. Playhead: 2 px `@accent_color` vertical line with a small triangle in the ruler; it follows the engine position (frame clock tick, no animation easing).

Grid:

- Row backgrounds: white-key rows `view_bg_color`; black-key rows `view_bg_color` mixed with `view_fg_color` at 5 percent; in-key rows (when Stay in Key is on) get an extra 8 percent accent tint, out-of-key rows are 15 percent dimmer. Horizontal lines at each octave (C) at 20 percent opacity.
- Vertical lines from `grid_lines`: bar level 2 at 30 percent, beat level 1 at 18 percent, subdivision level 0 at 8 percent (all x2.5 in high contrast).
- Notes: rounded rectangles (radius 3 px), fill = channel color at velocity-dependent opacity (0.55 + 0.45 x vel/127), 1 px darker border (our rule: the border keeps notes visible on any channel color, also in high contrast where the border becomes 1 px `window_fg_color`). Selected notes: 2 px `@accent_color` outline and a resize handle mark (3 short vertical lines) at the right edge.
- Note names on notes: when the note is at least 30 px wide and 14 px high, draw the pitch name ("C#4") at the note's left, 10 px (`.caption` equivalent, about 0.82 em), in a readable contrast color chosen as black or white by luminance of the note fill (our rule: compute from the fill, do not use fixed white).
- Overlapping notes at the same key are drawn in id order (later on top), matching `hit_note` in `roll_logic.rs`.
- Step-origin notes: a step note that has not been edited in the piano roll draws a small dot at its left edge (so users know the Steps section owns it); moving or resizing it removes the dot (SPEC 17.2 fmt-3).

Interactions:

| Input | Effect |
|---|---|
| Click empty grid | Add a note of the current default length (the last used length, or one snap unit) at the snapped position and key; the note is selected; previews |
| Click a note body | Select it (Ctrl+click adds to the selection, Shift+click range in time) |
| Drag a note body | Move in time and pitch, snapped (Shift = no snap, Ctrl = drag a copy). One undo entry |
| Drag a note's right edge (6 px zone, `EDGE_PX`) | Resize; cursor `ew-resize` |
| Drag on empty space with Shift | Rubber-band selection |
| Double-click a note | Delete it (also keyboard Delete). Chosen over right-click-delete because right-click is for menus in GNOME (our rule) |
| Right-click / Menu key | Popover menu: "Delete", "Duplicate", "Set Velocity...", "Quantize" |
| Scroll | Vertical scroll |
| Shift+scroll | Horizontal scroll |
| Ctrl+scroll | Zoom time anchored under the pointer (`zoom_x`) |
| Ctrl+Shift+scroll | Zoom rows (`zoom_y`) |
| Middle-drag | Pan |
| Velocity lane: drag a stem | Sets that note's velocity; dragging across stems while holding Shift sets each one (stem hit radius 6 px, `STEM_HIT_PX`) |

Keyboard (SPEC 11, with decision 2):

| Key | Effect |
|---|---|
| Arrow keys | Move the cursor cell (time step = snap unit, row = semitone); the roll scrolls to keep it visible (`reveal_key`, `reveal_tick`) |
| Return | Add a note at the cursor, or remove the note under the cursor |
| Shift+Left/Right | Resize the selected notes by one snap unit |
| Ctrl+Left/Right | Move selected notes by one snap unit in time |
| Ctrl+Up/Down | Move selected notes one semitone |
| Ctrl+Shift+Up/Down | Move selected notes one octave |
| + / - (when notes selected, plain keys, no text entry focused) | Velocity +-8 |
| Ctrl+A / Shift+Ctrl+A | Select all / deselect all |
| Delete | Delete selected notes |
| Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+U | Copy, cut, paste, duplicate |
| Page Up / Page Down | Scroll one octave |
| Home / End | Scroll to the pattern start / end |

Accessibility: role `generic` with label "Piano roll for Kick" (SPEC 11; consider `grid` if the AT-SPI role proves more useful in testing). Cursor moves update `accessible-label` to "C4, beat 2.3, no note" or "C4, beat 2.3, note, length 1/8, velocity 96", and `accessible-value-text` likewise. A live-region style announcement (set `accessible-description`) when a note is added or removed: "Note added at C4, bar 1 beat 2".

Empty states:

- No channel selected: `AdwStatusPage.compact`, icon `document-edit-symbolic`, title "Pick a Channel", description "Choose a channel in Steps to draw its notes here."
- Channel has no notes: the roll is shown (empty grid is the editor), plus an overlay hint label `.ldaw-hint` at the center (`GtkLabel.dim-label`, not interactive, disappears at the first note): "Click the grid to add a note." This hint is the only text drawn over content; it uses `window_fg_color` at 60 percent on `view_bg_color` and is exempt from "no text over backgrounds" because the background is flat.

Non-note channels: if the selected channel is a drum sampler in one-shot mode, the roll is replaced by `AdwStatusPage.compact`, icon `audio-x-generic-symbolic`, title "This Sound Plays on Steps", description "Use the step grid for drum sounds, or switch the channel to Pitched to play notes." with button "Switch to Pitched". (Sampler modes are SPEC 15.1.)

### 3.5 Mixer view

`AdwViewStack` page "mixer". Layout: horizontal `GtkBox`: a `GtkScrolledWindow` (horizontal only, `hscrollbar-policy=automatic`, kinetic) holding a `GtkBox` of strips, then a separator, then the pinned master strip. Returns (effect return tracks, SPEC 15.5) sit after the channel strips and before the separator; a flat "Add Return" button ends the scrolling box.

Strip (stock composition; custom: knob? no; custom: meter only). Width 96 px (80 at `bp-compact`, 88 at `bp-narrow`). Top to bottom:

```
+----------+
| ==color==|   4 px bar (identity)
| Kick     |   GtkEditableLabel, 1 line, F2 to edit, `.heading`
| Inserts  |   up to 4 slots + empty "Add Effect" row (see below)
| [slot 1] |
| [slot 2] |
| [ + ]    |
| Sends    |   AdwExpanderRow-like collapsed row "Sends" (expand shows up to 4 GtkScale, one per return)
| Pan      |   horizontal GtkScale -100..100, mark at 0, label "L" "R" at ends via marks
| [ meter | fader ]   LdawMeter beside GtkScale vertical (inverted, range -60..+6 dB, mark at 0 dB)
|  -3.2 dB |   `.numeric.caption` value, editable by double-click (GtkEditableLabel)
| [Mute][Solo]
+----------+
```

Stock widgets everywhere except the meter. The fader is a real `GtkScale` (vertical) so keyboard, accessibility, and focus work for free; we only style it with `ldaw-fader` (thicker 6 px trough, 20 px high slider, `color` from the palette) and set marks at 0 dB and -6, -12, -24, -48 dB drawn as tick labels `.caption` on the strip. Value mapping is dB with a curve, not linear gain (log taper): position p in 0..1 maps to dB with 0 dB at 75 percent travel; min "-inf" shown as "-inf".

- Fader keys: Up/Down 0.5 dB, Shift+Up/Down 0.1 dB, Page Up/Down 3 dB, Home = 0 dB, End = -inf. Double-click on the slider resets to 0 dB. Scroll over the fader: 0.5 dB per notch (only when focused, to avoid accidental changes while scrolling the mixer; our rule). Shift+drag is fine mode. Real-time feedback while dragging ([Sliders](https://developer.gnome.org/hig/patterns/controls/sliders.html)): the numeric readout updates live.
- Pan: `GtkScale` horizontal with marks "L", center, "R"; double-click resets; readout in the tooltip and in accessible value text ("30 percent left"). A visible center detent mark.
- Mute and Solo: two `GtkToggleButton`s labelled "Mute" and "Solo" (`.caption`, 44 px each; labels not abbreviated, for beginners). Checked state is visible by the pressed look; additionally a muted strip dims (fader and meter at 40 percent) and a soloed strip gets a 2 px accent outline; strips silenced by someone else's solo show the text "Silent" in `.caption.dim-label` above the meter (text, not color). Mute shortcut when strip focused: M; Solo: S (strip-local single-key shortcuts; our rule, consumed only when the strip has focus and no text entry is editing).
- Inserts: each slot is an `GtkBox` with a `GtkToggleButton.flat` (bypass, `media-playback-pause-symbolic` stand-in "Bypass Effect") and a `GtkButton.flat` with the effect name that opens the effect's controls in the Inspector "sound" page (for CLAP inserts also "Expert Window"). Empty slot: a `GtkMenuButton.flat` "Add Effect" with `list-add-symbolic` opening a popover menu: "EQ", "Compressor", "Saturator", "Reverb", "Delay", "Limiter", then "Plugin..." (SPEC 15.5). Order is signal order top to bottom; reorder by drag (keyboard: Alt+Up/Down on the slot). Sidechain input is chosen in the compressor's controls in the Inspector, not on the strip.
- Sends: collapsed by default into one row "Sends" with a chevron. Expanded: one labelled mini `GtkScale` per return ("Reverb", "Delay"), plus a pre/post `GtkToggleButton` "Pre". An empty set shows the dim text "No returns yet" and a flat "Add Return" button.
- Master strip: same layout without inserts' add menu (limiter fixed first and shown), no sends, name "Master", wider meter (stereo, two bars) and a clip lamp.
- Right-click strip: "Rename", "Change Color", "Remove Track", "Reset Fader". Remove has toast "Track removed - Undo".

Meter: custom `LdawMeter` (8 px per channel, height fills the strip's meter area). Draw only changed rect; redraw at the frame clock, not faster. Reads the engine's meter ring (SPEC 4.4). Colors (named colors, see section 4): up to -18 dBFS `success_color`, -18 to -6 `success_color` brightened, -6 to -1 `warning_color`, above -1 `error_color`. Segmented (3 px segments with 1 px gaps) so the level reads as a bar length. Peak hold: 2 px line, 1.5 s hold then fall. Clip lamp: a 6 px rectangle at the meter's top that latches when peak is at or above 0 dBFS, drawn with `error_bg_color`; click it to reset (tooltip "Clipped - Click to Reset"); it also sets accessible description "Clipped". Scale: tick marks at 0, -6, -12, -24, -48 dB. Not color only: segment lengths and the numeric peak readout (`.caption.numeric`, "-1.2" under the meter) carry the level.

Mixer empty state: the master strip always exists, so the view is never empty. If a project has no channels: `AdwStatusPage.compact` replaces the scrolling strips with icon `audio-volume-high-symbolic`, title "No Tracks Yet", description "Tracks appear here when you add channels." (master stays visible at the right).

Accessibility: each strip is a `GtkBox` with role `group` and label "Mixer strip: Kick". Faders and pans are real sliders. The meter has role `meter` (GTK 4.? supports `GTK_ACCESSIBLE_ROLE_METER` in recent versions; fall back to `progress-bar`) with value text "minus 12 decibels" updated at 4 Hz only (do not flood the screen reader).

### 3.6 Song view (arrangement, SPEC 15.6)

Custom widget `LdawPlaylist` (named `LdawSongCanvas` in code) inside the page, plus stock toolbar and track header column.

```
GtkBox vertical
  GtkBox.toolbar "song_toolbar":  [Brush: Pattern 1 v]  [Snap: Bar v]  [Zoom - +]  ...  [Export Audio...]
  GtkBox horizontal
    track headers (GtkListView, 180 px; 140 at bp-compact)
    GtkOverlay -> LdawSongCanvas (ruler 24 px, loop strip 6 px, lanes 48 px each)
```

Track header row: color bar, name (`GtkEditableLabel`), Mute and Solo flat toggles (icons `audio-volume-muted-symbolic` and a text "S"? use labelled `GtkToggleButton`s "Mute" and "Solo" at `.caption` so they match the Mixer), height 48 px (`touch` 52). Under the last row: "Add Track" flat button.

Canvas:

- Time axis in bars; default 24 px per bar (adjustable from 4 to 200); ruler shows bar numbers every N bars depending on zoom. Loop strip: drag to set a loop region in song mode (SPEC 15.6 "loop region optional").
- Clips: rounded rectangles filled with the clip's pattern color at 85 percent, 1 px border, height 40 px in a 48 px lane. Inside: pattern name (`.caption`, bold weight), and a mini preview of that pattern's notes (tiny horizontal bars scaled to the clip, drawn in a lighter shade). When the clip is longer than the pattern, a thin dashed divider marks each repeat.
- Playhead: 2 px accent line.
- Interactions: click an empty lane cell to place a clip of the brush pattern with its natural length (snapped to Snap); click a clip to select; drag moves (snapped, across lanes); drag the right edge resizes (snaps to the pattern length multiples when Ctrl is not held; free when Shift is held); Ctrl+drag copies; Delete removes; Ctrl+A/Shift+Ctrl+A select; Ctrl+C/V/U copy/paste/duplicate; double-click a clip opens that pattern in the Pattern view (switches page and sets the current pattern). Right-click clip: "Open Pattern", "Duplicate", "Delete", "Change Pattern" submenu is avoided (3-6 items rule, no nested menus; [Menus](https://developer.gnome.org/hig/patterns/controls/menus.html)); changing the pattern is done by selecting the clip and choosing another pattern in the Brush dropdown ("Apply to Selected" row at the top of the popover appears only when a clip is selected).
- Keyboard: arrows move the cursor (lane, bar); Return places or removes the brush clip at the cursor; Shift+arrows resize; Ctrl+arrows move the selected clip by one snap unit horizontally or one lane vertically; Page Up/Down scroll one screen.
- Scroll rules same as the roll (Ctrl+scroll zooms time).
- Pattern mode versus Song mode is the transport bar's mode toggle (section 5); in Pattern mode the canvas dims clips outside the current pattern's clips (no change in data).

Export Audio panel: a `GtkPopover` from "Export Audio..." in `song_toolbar` (also reachable from the menu, which then shows the Song view and opens the popover): rows (`AdwPreferencesGroup` style `GtkListBox.boxed-list` in the popover): `AdwComboRow` "Format" (WAV), `AdwComboRow` "What to Export" ("Whole Song", "Current Pattern", "Loop Region"), `AdwSwitchRow` "Normalize Loudness", then a suggested `GtkButton` "Export" opening `GtkFileDialog` (save). Progress is a `GtkProgressBar` in the same popover plus a toast on completion with button "Show in Files". Export runs as a job (SPEC 17.1) and never blocks the UI.

Empty state: no clips and no tracks: `AdwStatusPage.compact` centered in the canvas: icon `view-continuous-symbolic`, title "No Song Yet", description "Place your patterns on the timeline to build a full song." Button `suggested-action pill` "Add Pattern to Song" (places the brush pattern at bar 1 of track 1 and focuses it). Tracks exist but no clips: empty lanes show a faint dashed outline cell at bar 1 and the same hint in `.dim-label` ("Click a lane to place Pattern 1.").

Accessibility: canvas role `grid`; cursor label "Track 2, bar 5, clip Pattern 1, length 4 bars".

### 3.7 Instrument panel (Inspector, "Sound" page)

Located in the right utility pane `Inspector` (`AdwViewStack` page "sound"). Rationale: it affects the selected channel and is secondary to the main view ([Utility Panes](https://developer.gnome.org/hig/patterns/containers/utility-panes.html)). It is the same place for every instrument, native or plugin (SPEC 15.10.3).

Layout (`AdwToolbarView`):

```
top bar: GtkBox.toolbar  [prev preset <] [Preset name button, expands] [next preset >] [Vary]
content: GtkScrolledWindow > AdwClamp (max 420) > GtkBox vertical (spacing 18)
    ChannelHeader:  [color dot] Channel name (heading) / engine name .caption.dim-label
    MacroGrid:  GtkGrid 4 columns x 2 rows of MacroKnob (8 knobs)
    "More Controls" AdwExpanderRow-in-boxed-list
    "Expert Window" button (visible for CLAP; for native instruments: hidden)
bottom: AdwViewSwitcherBar (sound | agent | history)
```

- Preset name button: `GtkButton.flat` showing the preset's name (e.g., "Dark Bell"); activating it opens the sound browser sidebar focused on this channel's role (F9) so the user hears alternatives (SPEC 15.10.1). Prev/next: `go-previous-symbolic`, `go-next-symbolic`, tooltips "Previous Sound" and "Next Sound"; they audition (debounced 150 ms, SPEC 17.2 ux-5) and keep the loudness matched. "Vary" (`GtkButton` with label "Vary", tooltip "Try a Small Random Change") applies random changes within macro ranges; one undo entry; toast "Sound varied - Undo".
- MacroKnob: custom `LdawKnob` (below). Eight knobs with the names from the preset ("Tone", "Punch", "Grit", "Length", "Space", "Wobble", "Width", "Brightness"; sampler: "Pitch", "Tone", "Punch", "Length", "Grit", "Space"). Fewer than eight: the grid shows only those. A knob has a caption (`.caption`) below and a value readout (`.caption.numeric.dim-label`, "64") that appears on hover, focus, and while dragging. Knob size 48 px; touch 56 px. 72 px column width fits 4 columns in 340 px minus padding.
- "More Controls": a `GtkListBox.boxed-list` containing an `AdwExpanderRow` "More Controls" (collapsed by default). Inside, native instrument parameters as `AdwActionRow`s with suffix `GtkScale` (width 160) and a value label; waveform as `AdwComboRow`; voice/glide toggles as `AdwSwitchRow`. For CLAP plugins without a native macro map, "More Controls" lists the plugin's exposed parameters (first 32, searchable via an `AdwEntryRow`? no: a `GtkSearchEntry` row at the top) so even unmapped plugins have an inline path.
- Expert Window: `GtkButton` labeled "Expert Window" with `window-new-symbolic` prefix (tooltip "Open the plugin's own controls"). Opens the CLAP GUI window (SPEC 9.2). If the plugin has no GUI or Wayland embedding is unsupported (SPEC 9.5): the button is insensitive and the row below says (dim) "This plugin has no window of its own." If the GUI is a separate toplevel, it is a normal window, not a dialog.
- Insert and effect controls: when the selection is a mixer insert, the same page shows that effect's knobs (EQ bands, compressor, etc.) with the same layout.
- Empty state (no channel selected): `AdwStatusPage.compact`, icon `audio-x-generic-symbolic`, title "No Sound Selected", description "Choose a channel to change its sound."

Custom knob `LdawKnob`:

- 48 px circle: 270 degree arc track in `view_fg_color` at 15 percent; value arc in `@accent_bg_color`; pointer line in `window_fg_color`; no skeuomorphic gradients or shadows (flat, GNOME style).
- Mouse: vertical drag (200 px for full range), Shift for fine, scroll 2 percent (Shift 0.5 percent), double-click resets to default, right-click menu "Reset to Default" and (for CLAP parameters) "Show in Plugin".
- Keyboard: arrows 1 percent, Page Up/Down 10 percent, Home/End min/max, Return opens an inline number entry (`GtkEditableLabel`) for exact typing.
- Accessible role `slider` with `value-min`, `value-max`, `value-now`, `value-text` ("64 percent") and label "Tone". Because a knob changes only a `ParamTable` value, no recompile happens while dragging (SPEC 17.1 rt-1); each gesture is one undo entry.
- Focus ring: widget has `focusable=true`; CSS `ldaw-knob:focus-visible { outline: 2px solid alpha(@accent_color, 0.8); outline-offset: 2px; border-radius: 50%; }`. Verify in the first prototype that GTK paints the CSS outline for a custom widget with no custom snapshot code; if not, draw a 2 px ring in `snapshot()` from the palette.

### 3.8 Sound browser (left utility pane, SPEC 15.10)

`browser_split` sidebar, `AdwToolbarView`:

```
top bar: GtkSearchEntry "Search sounds"   (placeholder text; Ctrl+F focuses it when the pane is open)
         GtkScrolledWindow(horizontal, no vertical)  -> GtkBox of role filter toggles (.pill? use plain .flat toggles):
           "All" "Drums" "Bass" "Melody" "Pads" "FX" "Loops" "Vocals"
content: GtkStack
    page "results": GtkListView of SoundRow
    page "empty-results": AdwStatusPage.compact
    page "empty-library": AdwStatusPage.compact
bottom bar: GtkBox.toolbar [Add Sound Folder...] [style dropdown: "Any Style" / Phonk / Trap / Boom Bap / Lo-fi / House]
```

SoundRow (`GtkListView` item, 52 px high): prefix = `GtkButton.flat.circular` play (`media-playback-start-symbolic`, tooltip "Preview Sound", toggles to stop while playing); title = sound name (`.heading`, 1 line); subtitle = "808 - Phonk" or "Kick - Built-in" (`.caption.dim-label`, role first, engine name last per SPEC 15.10.1); suffix = for local-only packs the icon `computer-symbolic` with tooltip "On this computer only (not saved in shared projects)" (SPEC 17.2 license-1); for sounds already in the project nothing.

Interactions:

- Single click or arrow-key selection auditions the sound (SPEC 15.8.3) through the preview slot, debounced 150 ms; auditions use the project's key and tempo (SPEC 15.10.2). Return or double-click "adds as channel" (new channel with this sound, toast "Added Kick" with button "Undo"). Space re-previews (list rows consume Space, decision 2).
- Drag a row: drag source offering the sound id; targets: Steps empty area, "Add Channel" row, channel headers, piano roll (not accepted: no meaning), Song (not accepted for instrument presets; accepted for loops/audio: creates a channel plus a clip, deferred to a later milestone). Drag icon is the row icon with the name; cursor feedback via standard `dnd` cursors; if a drop target is invalid, no highlight (visible refusal).
- Role filter: toggles act as radio (one at a time), "All" default. Search is substring over name, tags, role, genre. Result sort: relevance, then name.
- Filter changes limit audition churn (SPEC 17.2: 200 rapid row changes cause at most 2 swaps).

Empty states:

- No results: `AdwStatusPage.compact`, icon `edit-find-symbolic`, title "No Sounds Found", description "Try another word or clear the filters." Button `GtkButton.pill` "Clear Filters".
- Library not yet built or no packs: icon `folder-music-symbolic`, title "No Sounds Yet", description "Add a folder with your samples, or install a sound pack." Button `suggested-action pill` "Add Sound Folder...". Analysis in progress (SPEC 15.10): a `GtkProgressBar` in the bottom bar and the label "Analyzing 120 of 480 sounds" (`.caption.dim-label`); rows appear as they are analyzed.

Keyboard: F9 toggles the pane; focus lands in the search entry when opened with Ctrl+F or when typing in the list (type-to-search via `GtkSearchEntry::set_key_capture_widget` is avoided because Space and letters are shortcuts elsewhere; capture only while the pane has focus).

### 3.9 Agent activity panel and approval banner (SPEC 16, 17.1)

Persistent state belongs in a banner; one-off events in toasts ([Banners](https://developer.gnome.org/hig/patterns/feedback/banners.html): "Banners do not automatically hide, and are displayed for ongoing periods of time"; [Toasts](https://developer.gnome.org/hig/patterns/feedback/toasts.html)).

Approval banner: `AdwBanner "agent_banner"` as a top bar under the transport bar; hidden unless approvals are pending.

- One pending: title "An agent wants to <verb>" in sentence form with a short object: "An agent wants to add the plugin Surge XT". Multiple: "3 agent requests need your approval". Session request (SPEC 17.1 mcp-8): "An agent wants to control LibreDAW".
- Button: "Review" (`AdwBanner.button-label`). It opens the Inspector on the "agent" page and focuses the first pending row. We use "Review" instead of Allow or Deny because `AdwBanner` has exactly one button and a one-click approval hides the details a privileged action needs (SPEC 17.1 trust model). Keyboard: the banner button is the first stop in the window's Tab order after the header bar and transport bar, so one Tab sequence reaches it.
- No timeout display in the banner (static). The 60 s wait (SPEC 17.1) is shown in the pending row. On timeout the banner changes: the request returns `denied` and the banner disappears; a toast says "Agent request timed out".
- Never shows modal dialogs. SPEC rule 15.8.6 ("no modal dialogs except these approvals") is satisfied by this non-modal mechanism, as SPEC 17.1 says.

Agent activity page (Inspector page "agent", icon `libredaw-agent-symbolic`):

```
AdwToolbarView
  content: GtkScrolledWindow > AdwClamp > GtkBox vertical
    AdwPreferencesGroup "Agent Session"       description "Agents can edit this project while this is on."
       AdwActionRow  title "Connected agent"  subtitle "claude-code - started 12:03"   suffix: GtkButton "Disconnect" (destructive-action? no: flat, label "Disconnect")
       AdwSwitchRow  title "Allow Agent Control"  (session scope; off by default; label sentence form)
    AdwPreferencesGroup "Waiting for You"      (only when pending)
       AdwActionRow per request: title "Add plugin Surge XT", subtitle "Waiting - 42 s left" (.numeric)
            suffix: [Deny] [Allow] buttons (Allow = suggested-action, Deny = flat)
    AdwPreferencesGroup "Recent Activity"
       AdwActionRow per commit: title = commit description ("Moved 4 notes in Pattern 2"), subtitle = "agent:claude-code - 12:05"
            suffix: GtkButton.flat "Undo" for the agent's own last commits (SPEC 17.1 mcp-5: undo only moves over the agent's own commits)
```

- The "Allow" button inside the row is the explicit human click that SPEC 17.1 requires. The agent cannot supply it.
- Text in rows is untrusted (SPEC 17.1): render as plain text (`use-markup=false`), ellipsize at 64 characters, no links.
- Empty state: `AdwStatusPage.compact`, icon `libredaw-agent-symbolic` (stand-in `network-workgroup-symbolic`), title "No Agent Connected", description "Connect an agent with libredaw-mcp, then allow it here." Link button `GtkLinkButton` "How to Connect an Agent" to help.
- Agent changes appear as undo commits authored by the agent (SPEC 15.11); the Pattern, Song, and Mixer switcher buttons get `needs-attention` while the user is on another page.
- When an agent is active and editing in the open pattern, the cells it changes flash with a 600 ms accent outline (disabled when `gtk-enable-animations` is false). A 1 s toast is not used for each edit (too many); only a single toast "Agent changed 12 things - Undo" after a request batch finishes.
- Busy and stale errors from MCP (SPEC 17.1 mcp-5) are shown only in the activity list (subtitle "Waited for you to finish editing"), never as toasts.

### 3.10 History page (Inspector page "history")

Not requested in the brief but needed by SPEC 15.11 and by "undo works for everything" (SPEC 15.8.5). Content: `GtkListBox.boxed-list` of named versions (`AdwActionRow`: name, subtitle time, suffix "Restore" button), a flat "Save Version" `GtkButton.pill` at the top with an `AdwEntryRow` for the name (apply on Enter), and under it a collapsed "Show Full History" `AdwExpanderRow` listing commits. "Restore" is PRIVILEGED for agents only; for the user it is one click plus a toast "Restored <name> - Undo" (restore is itself a commit, never destructive). Empty state: `AdwStatusPage.compact`, title "No Saved Versions", description "Save a version before you try something big. You can always go back."

### 3.11 Preferences (`AdwPreferencesDialog`)

Pages: "Audio" (`AdwComboRow` "Output Device", `AdwComboRow` "Buffer Size" values 64, 128, 256, 512, 1024, `AdwActionRow` "Sample Rate" read-only, SPEC 17.1 mcp-2 enum), "Appearance" (`AdwComboRow` "Color Scheme": Follow System, Light, Dark), "Sounds" (list of sound folders with "Add Folder...", `AdwSpinRow` "Sample Memory (GB)", SPEC 17.2 rt-5), "Agents" (`AdwSwitchRow` "Allow Agent Control for This Session" mirrors the activity page). Search enabled. It is an `AdwDialog` presented from the window, a deliberate user action. Audio device changes apply immediately with a toast on failure ("Could not use that device - using the default").

### 3.12 Errors and toasts catalogue

All errors are `AdwToast` unless they block the whole app. Titles are short, sentence-form text, no trailing period, optional button label is a verb.

| Event | Toast title | Button |
|---|---|---|
| Autosave or save failed | "Could not save the project" | "Details" (opens an `AdwAlertDialog` with the message and a "Try Again" response; this is the one blocking error and it stays because the window cannot close with unsaved work, SPEC 11) |
| Untitled saved on previous exit | "Your last project was saved to Music/LibreDAW" | "Show in Files" |
| Missing sample | "A sound file is missing and plays silence" | "Locate..." |
| Audio device lost | "Audio device disconnected - using the default output" | none |
| Channel / track removed | "Channel removed" | "Undo" |
| Sound added | "Added Kick" | "Undo" |
| Export finished | "Export finished" | "Show in Files" |
| Plugin crash (post-MVP) | "Plugin stopped working and was turned off" | "Details" |
| Agent timeout | "Agent request timed out" | none |

## 4. Custom-drawn widgets

Custom widgets: `LdawStepGrid`, `LdawPianoRoll`, `LdawSongCanvas`, `LdawMeter`, `LdawKnob`, `LdawClipPreview` (mini note map used inside clips). Everything else is stock. All are `GtkWidget` subclasses (`ObjectSubclass`, exempt per SPEC 11) whose logic is plain structs: `Viewport` and `roll_logic` already exist; add `grid_logic`, `song_logic`, `knob_logic`, `meter_logic`, `palette`.

### 4.1 Colors from the libadwaita palette

Rule: no hex literal in drawing code. Every color comes from a named color of the active style, so light, dark, high-contrast, and a user's accent override all work ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html): use "existing style classes and color variables that automatically adjust across light, dark, and high-contrast modes").

Named colors available in libadwaita 1.5 that we use:

| Role in our widgets | Named color |
|---|---|
| Grid, roll, canvas background | `view_bg_color` |
| Lines, off cells, ticks | `view_fg_color` (alpha-mixed in code) |
| Panel and strip background | `window_bg_color`, `card_bg_color` |
| Sidebar background | `sidebar_bg_color` (from `AdwOverlaySplitView` styling, not set by us) |
| Playhead, selection outline, value arcs, cursor | `accent_bg_color` for fills, `accent_color` for strokes and text |
| Text on fills | `accent_fg_color`, `window_fg_color`, `view_fg_color` |
| Meter green, amber, red | `success_color`, `warning_color`, `error_color` (strokes and fills on `view_bg_color`); `error_bg_color` for the clip lamp |
| Channel identity colors | palette colors `blue_3`, `green_4`, `yellow_5`, `orange_3`, `red_2`, `purple_2`, `brown_2`, `teal` equivalents; choose 8 slots from the libadwaita palette (`blue_3`, `teal_3`, `green_3`, `yellow_3`, `orange_3`, `red_3`, `purple_3`, `brown_3`), and a 9th and 10th slot as `blue_5`, `purple_5`. Dark and light variants of each slot are picked at lookup time (tone +1 darker in light style so contrast with `view_bg_color` is at least 3:1; verify with a test) |

How to read them (gtk4-rs 0.11):

1. Each custom widget holds a `Palette` struct (plain struct of `gdk::RGBA`) built by one function `Palette::from_widget(&Widget)`.
2. `from_widget` calls `widget.style_context().lookup_color("view_bg_color")` and the others in the table. `StyleContext::lookup_color` is deprecated since GTK 4.10 with no replacement for named-color lookup in libadwaita 1.5, so wrap it in one `#[allow(deprecated)]` function in `palette.rs` and nowhere else. The foreground text color comes from `widget.color()` (the CSS `color` property), which is not deprecated.
3. Refresh the palette in the `css_changed` vfunc of each widget (called when the style changes, including dark and light switches, high-contrast, and CSS reloads), and also on `AdwStyleManager` notifications `dark`, `high-contrast`, and `accent-color` (the last exists only from libadwaita 1.6; guard with `#[cfg(feature = "v1_6")]`). After refreshing, call `queue_draw()`.
4. Never read the palette in `snapshot()` from the style context; `snapshot()` uses the cached struct. Drawing allocates nothing per frame beyond the snapshot nodes.
5. Derived colors (blends, translucent tints) are computed once per refresh: `mix(a, b, t)` in sRGB is acceptable for the 4 to 8 percent tints used for backgrounds.
6. Channel colors are stored in the document as an index 0..9, never as RGB, so a theme change recolors everything and exported project files stay theme-neutral.

High contrast: when `StyleManager::is_high_contrast()` is true, `Palette` switches to a "strong" set: line alphas multiplied by 2.5 (capped at 1.0), note and step outlines 1 px `window_fg_color`, off cells keep their outline at 60 percent, selection outlines 3 px, hatching used for out-of-scale marks and clip repeats, meter segments outlined. The same code path runs; only the constants differ.

Light and dark: both come from the named colors. The only per-style constants are channel color tone offsets and shadow strengths (we draw no shadows; notes and clips are flat).

Testing the palette without a screen: unit-test `Palette` math (contrast ratio of each channel color against `view_bg_color` in both styles, at least 3:1 for outlines, 4.5:1 for text on fills). Manual: `ADW_DEBUG_COLOR_SCHEME=prefer-dark`, `ADW_DEBUG_COLOR_SCHEME=prefer-light`, `ADW_DEBUG_HIGH_CONTRAST=1` (debug environment variables; verify they exist in the installed libadwaita) and `GTK_THEME=Adwaita:hc`. Screenshot tests run the same app under each setting at 1920x1080, 1280x720, and 360x640.

### 4.2 Minimum hit targets and sizes

These are our rules (the HIG pointer and touch page could not be fetched; they follow WCAG 2.2 target size minimum 24x24 CSS px and libadwaita's default button heights).

| Element | Pointer | Touch (`.touch`, `bp-narrow`) |
|---|---|---|
| Header bar and toolbar buttons | libadwaita default (about 34 px tall) | same |
| Step cell | 28x32 | 36x44 |
| Note resize edge | 6 px zone (`EDGE_PX`), widened to 12 px when the note is wider than 24 px | 14 px |
| Velocity stem hit radius | 6 px (`STEM_HIT_PX`) | 12 px |
| Knob | 48x48 | 56x56 |
| Fader slider | 20 px high, track 6 px, hit area 32 px | 44 px |
| Mute and Solo | 44x28 | 56x44 |
| Mixer strip | 96 wide (80 at `bp-compact`) | 88 |
| Song lane height | 48 | 52 |
| Piano key row | 18 default, min 14 | 22 |
| Splitter handle (paned) | `wide-handle` 8 px hit | 24 px |

If a note is narrower than 6 px (zoomed far out), edge resize is disabled and the whole note moves; the zoom keeps working so users can zoom in. Grid cells never go below 24 px wide; at that size steps scroll rather than shrink.

### 4.3 Focus rings

- Whole-widget focus (knob, meter reset, fader, clip lamp, any custom widget used as one control): CSS `outline` on `:focus-visible` (see the knob rule in 3.7). Keep the libadwaita look: 2 px, `alpha(@accent_color, 0.8)`, offset 2 px.
- In-widget focus (the step cursor, roll cursor, canvas cursor): draw a 2 px rounded rectangle in `Palette.accent` (stroke) only while the widget has `focus-visible`; additionally draw an inner 1 px `Palette.window_fg` ring when high contrast is on. Stroke width scales with the display scale factor. The cursor must stay visible against both on and off cells, so it is drawn outside the cell by 1 px.
- Focus order: one tab stop per grid (arrows inside), per canvas, per knob, per strip control. Tab never gets trapped; Esc (or Tab) leaves a grid.
- The focus ring is never replaced by hover color. Hover is a lighter tint of the cell, not an outline.

### 4.4 Accessible roles and names

| Widget | Role | Label | Value text |
|---|---|---|---|
| `LdawStepGrid` | `grid` | "Steps" | cursor cell: "Step 5 of Kick, on, velocity 96" |
| lane editor | `grid` | "Velocity lane for Kick" | "Step 5, velocity 96" |
| `LdawPianoRoll` | `generic` (SPEC 11) with label | "Piano roll for Kick" | cursor: "C4, beat 2.3, note, velocity 96, length 1/8" |
| `LdawSongCanvas` | `grid` | "Song timeline" | "Track 2, bar 5, Pattern 1, 4 bars" |
| `LdawKnob` | `slider` | knob name ("Tone") | "64 percent" or real unit ("-3 dB") |
| `LdawMeter` | `meter` if available, else `progress-bar` | "Level of Kick" | "minus 12 decibels", updated 4 Hz |
| Fader and pan | stock `GtkScale` (role `slider`) | "Volume of Kick", "Pan of Kick" | "-3.2 dB", "30 percent left" |
| Mute, Solo | stock toggle buttons | "Mute Kick", "Solo Kick" | state = pressed |

Implementation: set `accessible-label` with `update_property(&[AccessibleProperty::Label(..)])` and the value text with `AccessibleProperty::ValueText`; update on every cursor move and every committed value, not on every pointer motion frame. Provide `AccessibleRole::Application` only on the window as GTK does. Each custom widget implements `GtkAccessible` through the default; no custom AT-SPI work in MVP.

Names rule: "short and descriptive", overriding the default where the default is unhelpful ([Accessibility](https://developer.gnome.org/hig/guidelines/accessibility.html)).

### 4.5 Animation and performance

- Frame clock tick callback only while playing or while a gesture is active; idle UI draws nothing.
- Only visible rows, columns, and ticks are drawn (SPEC 11). `queue_draw_area` is not available in GTK4; split large widgets into layers: grid (static, cached into a `gsk::RenderNode` and re-recorded on change), cursor/selection layer, and playhead layer (a child widget drawn above). The playhead moves only the playhead layer.
- Respect `gtk-enable-animations`: no fades or flashes when disabled; selection and playhead remain.
- No blinking or flashing elements (seizure rule, [UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html)): the clip lamp is solid, the agent flash is a single 600 ms fade and is off with animations disabled.

---

## 5. Transport and time display

The transport bar is a `GtkBox.toolbar` (horizontal, spacing 6) as the second top bar of the project `AdwToolbarView`. It is the same height as a header bar (47 px wide screens, 44 px with `touch`).

```
[skip back] [Play/Stop] [Metronome] | 012:3:2  | [ 140 ] BPM | 4/4 | Key: C Minor v | (Pattern|Song) [Loop]        ...space...   [master meter]
```

| Item | Widget | Detail |
|---|---|---|
| Skip back | `GtkButton.flat` `media-skip-backward-symbolic` | Tooltip "Go to Start (Home)". Moves the position to pattern or song start |
| Play and Stop | `GtkToggleButton` `media-playback-start-symbolic` / `media-playback-stop-symbolic`, 44x34 | Tooltip "Play (Space)" / "Stop (Space)". `.suggested-action` until first play in a new project, then `.flat`; checked while playing. Stop keeps the position at where playback started (like most editors); a second Stop press (or Home) returns to start |
| Metronome | `GtkToggleButton.flat` | Icon `libredaw-metronome-symbolic` (stand-in `alarm-symbolic`). Tooltip "Metronome (Ctrl+M)". Pressed state is the checked look |
| Position | `GtkButton.flat` containing a `GtkLabel` with `.numeric.title-4` | Width fixed for the widest value so it never jitters (use `width-chars` and `.numeric` tabular figures). Text "012:3:2" = bar : beat : sixteenth, zero-padded to 3 digits for bars. Click toggles to time format "01:23.4" (minutes:seconds.tenths). Tooltip "Position - click to switch between bars and time". Accessible name "Position", value text "Bar 12, beat 3, step 2" |
| Tempo | `GtkSpinButton` (range 20 to 300, step 1, digits 1, `width-chars` 5, `.numeric`) followed by `GtkLabel.dim-label` "BPM" | Tooltip "Tempo". Numeric entry, scroll over it changes tempo only while it has focus (our rule). Right of it, `GtkButton.flat` "Tap" (tooltip "Tap the Tempo", label "Tap"), average of the last 4 taps; wide only |
| Time signature | `GtkMenuButton.flat` "4/4" | Popover with `AdwSpinRow`s "Beats per Bar" and a combo "Note Value" (kept simple: 2/4, 3/4, 4/4, 5/4, 6/8, 7/8, 12/8 as a list). Wide only |
| Key | `GtkMenuButton.flat` "Key: C Minor" | Popover with two `AdwComboRow`s "Root Note" and "Scale" (Major, Minor, plus "Chromatic (No Key)"). Drives Stay in Key (SPEC 15.10.5). Wide only |
| Play mode | two `GtkToggleButton`s in a `.linked` box: "Pattern" and "Song" | Radio behavior via `group`. Tooltips "Loop the Current Pattern" and "Play the Whole Song" |
| Loop | `GtkToggleButton.flat` `media-playlist-repeat-symbolic` | Tooltip "Loop (Ctrl+L)". In Pattern mode loops the pattern; in Song mode loops the loop region |
| Master meter | `LdawMeter` stereo, horizontal 96x10 | Always visible; click opens the Mixer view. Tooltip "Master Level - click to open the Mixer" |

At `bp-compact`, key, time signature, and play mode fold into one `GtkMenuButton.flat` `view-more-symbolic` (tooltip "Transport Settings") whose popover holds those rows as `AdwComboRow` and `AdwSpinRow`. At `bp-narrow`, the bar keeps skip back, Play/Stop, position, and a tempo `GtkMenuButton.flat` ("140") whose popover holds the spin button, "Tap", metronome, loop, and the other settings; undo and redo also appear in that popover while the Notes section is maximized.

Position update: the label updates from the engine position on the frame clock at most 30 Hz, only while playing, only when the displayed text changes. The position display and the tempo field are separate controls; typing a tempo does not require a dialog.

Tempo entry validation: invalid text reverts on focus out and the field gets the `.error` class for one second plus a toast "Tempo must be between 20 and 300 BPM" (no modal).

### 5.1 Keyboard shortcuts

Standard GNOME shortcuts are used where our function matches ([Standard Keyboard Shortcuts](https://developer.gnome.org/hig/reference/keyboard.html), [Keyboard](https://developer.gnome.org/hig/guidelines/keyboard.html)). Our own shortcuts avoid Alt and Super, use Ctrl+letter or function keys, and are listed in the Keyboard Shortcuts window grouped as "General", "Transport", "Editing", "View", "Steps", "Notes", "Song", "Mixer".

Global (window scope; ignored when the focused widget consumes the key, for example an entry or button):

| Keys | Action | Source |
|---|---|---|
| Space | Play / Stop | ours (decision 2) |
| Shift+Space | Play from start | ours |
| Home | Go to start | ours (Alt+Home is the HIG "Home" for navigation, not used here) |
| Ctrl+M | Metronome on/off | ours |
| Ctrl+L | Loop on/off | ours |
| Ctrl+N | New Project | HIG |
| Ctrl+O | Open Project | HIG |
| Ctrl+S | Save | HIG |
| Shift+Ctrl+S | Save As | HIG |
| Ctrl+E | Export Audio | ours |
| Ctrl+Z | Undo | HIG |
| Shift+Ctrl+Z | Redo | HIG |
| Ctrl+X, Ctrl+C, Ctrl+V | Cut, copy, paste | HIG |
| Ctrl+U | Duplicate | HIG |
| Ctrl+A | Select all | HIG |
| Shift+Ctrl+A | Deselect all | HIG |
| Delete | Delete selection | ours |
| Ctrl+F | Search sounds (opens the pane) | HIG (Find) |
| Ctrl++, Ctrl+-, Ctrl+0 | Zoom in, out, reset (time axis of the focused editor) | HIG |
| F9 | Show or hide Sounds | HIG (side pane) |
| Shift+F9 | Show or hide Inspector | ours |
| Ctrl+1, Ctrl+2, Ctrl+3 | Go to Pattern, Song, Mixer | ours |
| Ctrl+, | Preferences | HIG |
| Ctrl+? | Keyboard Shortcuts | HIG |
| F1 | Help | HIG |
| Alt+Return | Project Properties | HIG (Properties) |
| F2 | Rename the selected channel, track, pattern | ours |
| F10 | Open the primary menu | HIG |
| Ctrl+Q | Quit (saves first) | HIG |
| Ctrl+W | Close window (saves first) | HIG |
| Esc | Close popover or overlay sidebar; leave a grid | HIG |

Editor scope keys are in sections 3.3 (Steps), 3.4 (Notes), 3.6 (Song), and 3.5 (Mixer: M, S, arrows, Page Up/Down, Home, End on a strip). The shortcuts window and the in-app tooltips are generated from one table (`shortcuts.rs`) so they cannot drift.

Space handling (decision 2) in code: install the Space accelerator on the window through a `GtkShortcutController` (scope `Global`) with a condition callback that returns false when `window.focus()` is a `GtkEditable`, `GtkButton`, `GtkCheckButton`, `GtkSwitch`, `GtkListView` row, or `GtkDropDown`; custom editors do not consume Space.

### 5.2 Time and tempo display rules

- All time values use `.numeric` (tabular figures), so digits do not shift as they change.
- Positions in bars are 1-based ("Bar 1 beat 1"), ticks are never shown to users except as "step" (16th note) numbers.
- Tempo and swing show their units ("140 BPM", "50%").
- dB values show one decimal and a minus sign (U+2212 is not used; plain "-3.2 dB"), "-inf" below -60 dB.

---

## 6. Copy and naming

Capitalization follows the HIG ([Writing Style](https://developer.gnome.org/hig/guidelines/writing-style.html)):

- Header Capitalization: view titles, tab and page titles, button labels, switch labels, menu items, tooltips, section headings in lists ("Recent Projects").
- Sentence capitalization: labels of check boxes, radio buttons, sliders, text entries, field labels, combo box labels, body and description text, toast titles, banner titles, status page descriptions. Status page titles use Header Capitalization as in the HIG placeholders example "Empty Folder" ([Placeholders](https://developer.gnome.org/hig/patterns/feedback/placeholders.html)).
- Ellipsis "..." (plain three dots; the HIG uses the ellipsis character; use U+2026 in actual strings) only when a command needs more input before it runs.
- No periods on headings, single-sentence labels, and toast titles. Plain straight quotes in all strings.
- Verbs on buttons ("Add Channel", "Export", "Restore"), never "OK", "Yes", "No", or "Submit" ([Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)). Views and sections are nouns ([View Switchers](https://developer.gnome.org/hig/patterns/nav/view-switchers.html)).
- Every control has a tooltip (SPEC 15.8.3). Tooltips include the shortcut in parentheses when there is one.
- Terms: plain first. "Steps" not "step sequencer"; "Notes" not "piano roll" in labels (the accessible name still says "Piano roll"); "Sounds" not "presets"; "Vary" not "randomize"; "Stay in Key" not "scale lock"; "Swing" stays; "Snap" stays; "Ratchet" stays but always has the tooltip "Repeat the step 2, 3, 4, 6, or 8 times (hi-hat rolls)"; "Send" becomes "Sends" with the tooltip "How much of this track goes to an effect"; "Insert" is shown as "Effects"; "Return" is shown as "Effect Return" in tooltips and "Returns" in the Mixer; "Velocity" stays with tooltip "How hard the note is played"; "Quantize" is "Snap to Grid" in menus.

### 6.1 Every visible label (string table)

Strings marked (T) are tooltips; (A) accessible-only names; (D) status-page or description text.

Window and menus

| Where | String |
|---|---|
| Window title | "<Project Name> - LibreDAW" |
| Main menu button (T/A) | "Main Menu" |
| Menu | "New Project", "Open Project...", "Save", "Save As...", "Export Audio...", "Project Properties", "Preferences", "Keyboard Shortcuts", "Help", "About LibreDAW" |
| Header (T) | "Show Sounds (F9)", "Hide Sounds (F9)", "Undo (Ctrl+Z)", "Redo (Shift+Ctrl+Z)", "Show Inspector (Shift+F9)", "Hide Inspector (Shift+F9)", "An agent is connected" |
| View switcher | "Pattern", "Song", "Mixer" |

Start screen

| Where | String |
|---|---|
| Title (D) | "What do you want to make?" |
| Description | "Pick a style and press Play. You can change everything later." |
| Sections | "Start a Beat", "Recent Projects", "Demo Projects" |
| Cards | "Phonk", "Trap", "Boom Bap", "Lo-fi", "House", "Empty Project"; subtitle "140 BPM - 8 channels" |
| Card preview (T) | "Preview Beat" |
| Buttons | "Open Project..." |
| Row subtitle | "Edited 2 hours ago - 140 BPM", "Demo - 128 BPM - by LibreDAW" |
| Row menu | "Show in Files", "Remove from List" |
| Empty recent | "No Recent Projects" / "Projects you open appear here." |
| Templates missing | "Templates Missing" / "Reinstall LibreDAW or open a project from your files." |

Transport

| Where | String |
|---|---|
| Buttons (T) | "Go to Start (Home)", "Play (Space)", "Stop (Space)", "Metronome (Ctrl+M)", "Loop (Ctrl+L)", "Tap the Tempo", "Transport Settings", "Master Level - click to open the Mixer", "Position - click to switch between bars and time", "Tempo" |
| Labels | "BPM", "Tap", "4/4", "Key: C Minor", "Pattern", "Song" |
| Popover rows | "Beats per Bar", "Note Value", "Root Note", "Scale", "Major", "Minor", "Chromatic (No Key)" |
| Mode (T) | "Loop the Current Pattern", "Play the Whole Song" |
| Error toast | "Tempo must be between 20 and 300 BPM" |

Pattern view

| Where | String |
|---|---|
| Pattern strip | "Pattern 1" (menu button); (T) "Choose Pattern", "New Pattern", "Pattern Length in Bars"; labels "Bars", "Swing"; popover item "New Pattern"; (T) "Focus Steps or Notes"; "Pattern Settings" |
| Steps header | lane toggles "Velocity", "Pitch", "Ratchet"; (T) "Show Velocity Lane", "Show Pitch Lane", "Show Ratchet Lane" |
| Channel row (T) | "Mute Channel", "Edit Sound", "Edit Notes", "Channel Menu" |
| Channel menu | "Edit Sound", "Edit Notes", "Rename", "Change Color", "Duplicate", "Clear Steps", "Remove Channel" |
| Step menu | "Clear Step", "Set Velocity", "Set Pitch", "Set Ratchet" |
| Add channel | button "Add Channel"; menu "Drum Sound...", "Synth", "808 Bass", "Sampler", "Plugin..." |
| Empty channels | "No Channels Yet" / "Add a sound to start your beat. You can also drag one in from the sounds list." |
| Ratchet (T) | "Repeat the step 2, 3, 4, 6, or 8 times (hi-hat rolls)" |
| Toasts | "Channel removed" (Undo), "Sound changed" (Undo) |

Notes section

| Where | String |
|---|---|
| Toolbar | "Snap: 1/16" (value of combo; options "Off", "1/4", "1/8", "1/16", "1/32", "1/16 Triplet"); (T) "Snap to Grid"; button "Stay in Key" (T) "Highlight and snap to the project key"; (T) "Zoom Out (Ctrl+-)", "Zoom In (Ctrl++)", "Show Velocity Lane" |
| Menu | "Delete", "Duplicate", "Set Velocity...", "Snap to Grid" |
| Empty | "Pick a Channel" / "Choose a channel in Steps to draw its notes here." |
| Hint | "Click the grid to add a note." |
| Sampler mode | "This Sound Plays on Steps" / "Use the step grid for drum sounds, or switch the channel to Pitched to play notes." / button "Switch to Pitched" |
| Tooltips on keyboard | "C4" etc. (key name on hover) |

Mixer

| Where | String |
|---|---|
| Strip | "Mute", "Solo", "Pan", "Sends", "No returns yet", "Add Return", "Pre"; slot "Add Effect"; slot (T) "Bypass Effect"; "Silent" (when another track is soloed) |
| Effects menu | "EQ", "Compressor", "Saturator", "Reverb", "Delay", "Limiter", "Plugin..." |
| Strip menu | "Rename", "Change Color", "Remove Track", "Reset Fader" |
| Clip lamp (T) | "Clipped - Click to Reset" |
| Master | "Master" |
| Empty | "No Tracks Yet" / "Tracks appear here when you add channels." |
| Toast | "Track removed" (Undo) |

Song

| Where | String |
|---|---|
| Toolbar | "Brush: Pattern 1" (menu button), "Apply to Selected", "Snap: Bar", "Export Audio..." |
| Track header | "Mute", "Solo", "Add Track" |
| Clip menu | "Open Pattern", "Duplicate", "Delete" |
| Empty | "No Song Yet" / "Place your patterns on the timeline to build a full song." / button "Add Pattern to Song"; hint "Click a lane to place Pattern 1." |
| Export popover | rows "Format", "What to Export" ("Whole Song", "Current Pattern", "Loop Region"), "Normalize Loudness"; button "Export"; toast "Export finished" (Show in Files) |

Inspector

| Where | String |
|---|---|
| Switcher | "Sound", "Agent", "History" |
| Sound page | (T) "Previous Sound", "Next Sound"; button "Vary" (T) "Try a Small Random Change"; "More Controls"; "Expert Window" (T) "Open the plugin's own controls"; dim text "This plugin has no window of its own." |
| Macro names | "Tone", "Punch", "Grit", "Length", "Space", "Wobble", "Width", "Brightness"; sampler "Pitch", "Tone", "Punch", "Length", "Grit", "Space" |
| Empty | "No Sound Selected" / "Choose a channel to change its sound." |
| Toast | "Sound varied" (Undo) |
| History | button "Save Version"; row field "Version name"; button "Restore"; "Show Full History"; empty "No Saved Versions" / "Save a version before you try something big. You can always go back." |

Sound browser

| Where | String |
|---|---|
| Search | "Search sounds" |
| Filters | "All", "Drums", "Bass", "Melody", "Pads", "FX", "Loops", "Vocals"; style "Any Style", "Phonk", "Trap", "Boom Bap", "Lo-fi", "House" |
| Row (T) | "Preview Sound"; local-only (T) "On this computer only (not saved in shared projects)" |
| Buttons | "Add Sound Folder...", "Clear Filters" |
| Progress | "Analyzing 120 of 480 sounds" |
| Empty | "No Sounds Found" / "Try another word or clear the filters."; "No Sounds Yet" / "Add a folder with your samples, or install a sound pack." |
| Toast | "Added Kick" (Undo) |

Agent

| Where | String |
|---|---|
| Banner | "An agent wants to add the plugin Surge XT" / "3 agent requests need your approval" / "An agent wants to control LibreDAW"; button "Review" |
| Page groups | "Agent Session", "Waiting for You", "Recent Activity" |
| Rows | "Allow Agent Control", "Disconnect", "Allow", "Deny", "Undo", "Waiting - 42 s left" |
| Empty | "No Agent Connected" / "Connect an agent with libredaw-mcp, then allow it here." link "How to Connect an Agent" |
| Toasts | "Agent request timed out", "Agent changed 12 things" (Undo) |

Preferences

| Where | String |
|---|---|
| Pages | "Audio", "Appearance", "Sounds", "Agents" |
| Rows | "Output Device", "Buffer Size", "Sample Rate", "Color Scheme" (Follow System, Light, Dark), "Add Folder...", "Sample Memory (GB)", "Allow Agent Control for This Session" |
| Toast | "Could not use that device - using the default" |

General errors: "Could not save the project" (Details), "A sound file is missing and plays silence" (Locate...), "Audio device disconnected - using the default output", "Plugin stopped working and was turned off" (Details), "Your last project was saved to Music/LibreDAW" (Show in Files).

---

## 7. What not to do

Anti-patterns common in DAW UIs, with the reason from the HIG (or "our rule" when the HIG page was not reachable):

1. Menu bar with File, Edit, View, Track, Help. HIG: apps use the header bar and one primary menu with 3 to 12 items ([Menus](https://developer.gnome.org/hig/patterns/controls/menus.html), [Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)).
2. A header bar crammed with 20 buttons (transport, tools, zoom). HIG: "small number of controls", leave drag space ([Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)). Our transport lives in its own bar.
3. "Save changes?" on close. HIG: dialogs only for deliberate actions; prefer undo ([Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)); SPEC 11 close saves.
4. "Are you sure you want to delete this channel?" confirmations. HIG: "undo is typically a better option than a confirmation dialog" (same page). We use toasts with Undo.
5. Modal error boxes ("Audio device not found - OK"). HIG: toasts for non-critical errors (same page, [Toasts](https://developer.gnome.org/hig/patterns/feedback/toasts.html)); banners for persistent states ([Banners](https://developer.gnome.org/hig/patterns/feedback/banners.html)).
6. "OK / Cancel / Yes / No" buttons. HIG: label the affirmative button with a specific imperative verb ([Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)).
7. ALL CAPS labels, abbreviations as the only label, mixed capitalization. HIG capitalization rules ([Writing Style](https://developer.gnome.org/hig/guidelines/writing-style.html)); jargon harms beginners (SPEC 15.8).
8. A forced dark theme, or colors hard-coded in drawing code. HIG: respect system style, test high contrast ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html)).
9. Color as the only difference between muted and unmuted, on and off, clipped and fine. HIG: do not rely "solely on color" (same page).
10. Skeuomorphic knobs and faders (gradients, drop shadows, brushed metal, LED fonts). HIG: minimize custom styling, use style classes (same page). Our flat knob and stock `GtkScale` are enough.
11. Floating tool windows and undocked panels (piano roll in its own window, mixer in another). HIG adaptive layout: windows should resize smoothly and not rely on many small panes ([Adaptive Layout](https://developer.gnome.org/hig/guidelines/adaptive.html)); floating windows fail on 360 px and on tiling compositors. Only the plugin's own GUI is a separate window, because it is not ours.
12. Hover-only controls (buttons that appear on hover). HIG accessibility: everything must work by keyboard and be discoverable ([Accessibility](https://developer.gnome.org/hig/guidelines/accessibility.html)).
13. Right-click as the only way to reach a feature, or right-click to delete. Our rule: menus open on right-click (and the Menu key); every command also has a visible control or shortcut.
14. Mouse-wheel hijack (wheel always zooms, or changes a knob while scrolling past it). Our rule: plain wheel scrolls; Ctrl+wheel zooms; wheel on a fader or knob acts only while it has focus.
15. Keyboard shortcuts that use Alt or Super, or reassign standard ones (Ctrl+S not save, Ctrl+Z not undo). HIG: avoid Alt (access keys) and Super (system) ([Keyboard](https://developer.gnome.org/hig/guidelines/keyboard.html)); use the standard table ([Standard Keyboard Shortcuts](https://developer.gnome.org/hig/reference/keyboard.html)).
16. Custom window decorations, custom close buttons, custom scrollbars, custom context menus. HIG header bars and popover menus are the standard ([Menus](https://developer.gnome.org/hig/patterns/controls/menus.html)).
17. Sliders for open-ended values, or sliders with no number. HIG: avoid sliders for open ranges, give real-time feedback and a value ([Sliders](https://developer.gnome.org/hig/patterns/controls/sliders.html)). Tempo is a spin button; faders show dB.
18. Sidebar of 12 view icons. HIG: three to five views in a switcher, sidebar beyond that ([View Switchers](https://developer.gnome.org/hig/patterns/nav/view-switchers.html)). We have three.
19. A blank grid on first launch. HIG: placeholders must explain the empty state and offer the next action ([Placeholders](https://developer.gnome.org/hig/patterns/feedback/placeholders.html)).
20. Splash screens, tips-of-the-day dialogs, "Welcome" wizards, audio setup wizards on first run. HIG: never pop up a dialog unexpectedly ([Dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html)); SPEC 15.7: "New project never asks technical questions".
21. Tiny fixed fonts (9 px labels), fixed pixel sizes that ignore text scaling. Our rule and HIG accessibility spirit; use `sp` and the stock type classes (`.caption`, `.heading`, `.numeric`).
22. Flashing meters and blinking record or clip lights. HIG: avoid flashing or blinking elements ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html)).
23. A tab bar for views (like browser tabs) or a document tab strip of open projects. Tabs are for documents; our views are a view switcher; SPEC has one project per window.
24. Nested menus and long flat menus. HIG: 3 to 12 items, submenus 3 to 6, avoid nesting ([Menus](https://developer.gnome.org/hig/patterns/controls/menus.html)). We use popovers with sections and separate pages instead.
25. Label-only or suggested/destructive-styled buttons in the header bar. HIG header bar button rules ([Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)).
26. Disabling a control without a way to learn why. Our rule: insensitive controls keep a tooltip that says what is needed ("Nothing to undo", "This plugin has no window of its own").
27. Auto-opening dialogs from the agent (SPEC 16, 17.1). Agent requests are a banner plus a row; the human clicks "Allow" in the row. HIG: dialogs only in response to a deliberate action.

---

## 8. Wireframes

Not to scale. 1920x1080 frames are drawn at about 118 columns (one column is about 16 px). 360 px frames are drawn at 40 columns (one column is 9 px). Legend: `[x]` button, `(o)` toggle on, `( )` toggle off, `#` filled step or note, `.` empty step, `|>` playhead, `====` color bar.

### 8.1 Start screen, 1920x1080

```
+----------------------------------------------------------------------------------------------------------------+
|                                               LibreDAW                                          [Main Menu]    |
+----------------------------------------------------------------------------------------------------------------+
|                                                                                                                |
|                          +--------------------------------------------------------------+                      |
|                          |  What do you want to make?                                   |   (clamp 960 px)     |
|                          |  Pick a style and press Play. You can change everything later|                      |
|                          |                                                              |                      |
|                          |  Start a Beat                                                |                      |
|                          |  +--------+ +--------+ +--------+ +--------+ +--------+      |                      |
|                          |  | [>]    | | [>]    | | [>]    | | [>]    | | [>]    |      |                      |
|                          |  |  icon  | |  icon  | |  icon  | |  icon  | |  icon  |      |                      |
|                          |  | Phonk  | | Trap   | |BoomBap | | Lo-fi  | | House  |      |                      |
|                          |  |140 BPM | |140 BPM | | 90 BPM | | 78 BPM | |124 BPM |      |                      |
|                          |  +--------+ +--------+ +--------+ +--------+ +--------+      |                      |
|                          |  + Empty Project                                             |                      |
|                          |                                                              |                      |
|                          |  Recent Projects                                             |                      |
|                          |  +--------------------------------------------------------+  |                      |
|                          |  | [icon] Night Drive            Edited 2 hours ago - 140 BPM >|                    |
|                          |  | [icon] Untitled 3             Edited yesterday - 120 BPM  >|                    |
|                          |  +--------------------------------------------------------+  |                      |
|                          |  Demo Projects                                               |                      |
|                          |  +--------------------------------------------------------+  |                      |
|                          |  | [icon] Dark Bell Loop         Demo - 128 BPM - by LibreDAW >|                  |
|                          |  +--------------------------------------------------------+  |                      |
|                          |  [Open Project...]                                           |                      |
|                          +--------------------------------------------------------------+                      |
+----------------------------------------------------------------------------------------------------------------+
```

### 8.2 Pattern view, 1920x1080 (sound browser and inspector both shown)

```
+------------------------------------------------------------------------------------------------------------------+
|[Sn][<-][->]                           ( Pattern )  Song   Mixer                                [Ag][In][Main Menu]|
+------------------------------------------------------------------------------------------------------------------+
|[|<] [ > ] (o)Met | 012:3:2 | [140]BPM [Tap] | 4/4 | Key: C Minor v | (Pattern|Song) ( )Loop       master ||||||||| |
+----------------+-------------------------------------------------------------------------+-----------------------+
| Search sounds  | Pattern 1 v [+]  [4] Bars   Swing |-----o----| 20%                  [Focus]|  Inspector            |
| All Drums Bass |-------------------------------------------------------------------------| ---------------------- |
| Melody Pads FX | [Velocity][Pitch][Ratchet]        1 . . . 2 . . . 3 . . . 4 . . . |       |  [<]  Dark Bell  [>] [Vary]|
|----------------| ==== (o) Kick    [S][N] [# . . . # . . . # . . . # . . . ]         |   | ------------------------- |
| (>) Kick Hard  | ==== (o) Snare   [S][N] [. . . . # . . . . . . . # . . . ]         |   |  Kick  - Built-in         |
|    Drums-Phonk | ==== (o) Hat     [S][N] [# . # . # . # . # . # . # . # . ]         |   |                           |
| (>) 808 Long   | ==== (o) 808     [S][N] [# . . . . . # . . . # . . . . . ]         |   |   (o)    (o)    (o)   (o) |
|    Bass-Phonk  | ==== (o) Bell    [S][N] [. . . . . . . . . . . . . . . . ]         |   |  Tone   Punch  Grit  Length|
| (>) Dark Bell  |        [+ Add Channel]                                              |   |   (o)    (o)    (o)   (o) |
|    Melody      |  ---- velocity lane (Hat) ----------------------------------------   |   |  Space  Wobble Width Bright|
| (>) Open Hat   |      | | | | | | | | | | | | | | | |                                  |   |                           |
|    Drums       |-------------------------------------------------------------------------| |  v More Controls          |
|                | Notes   [Bell v]  [Snap: 1/16 v] [Stay in Key]  [-][+]  [Vel lane]      | |  [Expert Window]          |
|  (list, 52 px  |  C5 |      |   ####                  ####                              |   | ------------------------- |
|   rows,        |  B4 |------|----------|-----|-----------|--------|---------               |  [Sound][Agent][History]  |
|   scrolls)     |  A#4|      |                                                           |   |                           |
|                |  A4 |  ####|                                                           |   |                           |
|                |  C4 |   C#4 label on note   |>                                          |   |                           |
|                |  ...|       (20 rows at 18-20 px, C rows labelled "C3","C4","C5")        |   |                           |
|                |  vel|  | | | | | | | | | | | |  (64 px velocity lane)                   |   |                           |
| [Add Sound     |                                                                         |   |                           |
|  Folder...]    |                                                                         |   |                           |
+----------------+-------------------------------------------------------------------------+-----------------------+
```

Widths: browser 300, workspace 1280, inspector 340. Steps section about 40 percent of the height, Notes about 60 percent, divider draggable. `[Sn]` sounds toggle, `[In]` inspector toggle, `[Ag]` agent indicator (present only when an agent session is on), `[S]` Sound button, `[N]` Edit Notes button.

### 8.3 Song view, 1920x1080

```
+------------------------------------------------------------------------------------------------------------------+
|[Sn][<-][->]                            Pattern  ( Song )  Mixer                                [In][Main Menu]  |
+------------------------------------------------------------------------------------------------------------------+
|[|<] [ > ] (o)Met | 012:3:2 | [140]BPM | 4/4 | Key: C Minor | ( )Pattern (o)Song ( )Loop              master |||||||  |
+----------------+-------------------------------------------------------------------------+-----------------------+
| (sounds pane)  | Brush: Pattern 1 v   Snap: Bar v   [-][+]                [Export Audio...]|  (inspector pane)     |
|                |-----------------------------------------------------------------------| |                       |
|                | Track names (180)  | 1       5       9       13      17      21      25  |                       |
|                |                    | [loop strip ==================]                   |                       |
|                | ==== Drums  [Mute][Solo] | [Pat 1 .:.:.][Pat 1 .:.:.][Pat 2 .::.::.]  .... |                       |
|                | ==== Bass   [Mute][Solo] |            [Pat 3 ======= ][Pat 3 ======= ]     |                       |
|                | ==== Melody [Mute][Solo] |                         [Pat 4 : . : . : . ]    |                       |
|                | ==== FX     [Mute][Solo] |  .   .   .   .   .   .   .   .   .   .   .      |                       |
|                | [+ Add Track]            |   |>  (playhead)                                |                       |
+----------------+-------------------------------------------------------------------------+-----------------------+
```

Empty variant: canvas centered `AdwStatusPage` "No Song Yet" with the "Add Pattern to Song" button.

### 8.4 Mixer view, 1920x1080

```
+------------------------------------------------------------------------------------------------------------------+
|[Sn][<-][->]                            Pattern   Song  ( Mixer )                               [In][Main Menu]  |
+------------------------------------------------------------------------------------------------------------------+
|[|<] [ > ] (o)Met | 012:3:2 | [140]BPM | 4/4 | Key: C Minor | (o)Pattern ( )Song ( )Loop              master |||||||  |
+------------------------------------------------------------------------------------------------------------------+
| ==== Kick ===+ ==== Snare ==+ ==== Hat ====+ ==== 808 ====+ ==== Bell ===+ | ==== Reverb ==+ | ==== Master ===+  |
| Effects      | Effects      | Effects      | Effects      | Effects      | | Effects       | | Limiter        |  |
| [Saturator]  | [EQ]         | [ + ]        | [Compressor] | [ + ]        | | [Reverb]      | | [ + ]          |  |
| [ + ]        | [ + ]        |              | [ + ]        |              | |               | |                |  |
| > Sends      | > Sends      | > Sends      | > Sends      | > Sends      | |               | |                |  |
| Pan  L--|--R | Pan  L--|--R | Pan L--|-R    | Pan L--|--R  | Pan L-|--R   | | Pan L--|--R   | |                |  |
| |  ||  -6 dB  | |  ||        | |  ||        | |  ||        | |  ||        | | |  ||         | | ||  ||         |  |
| |  ||        | |  ||        | |  ||        | |  ||        | |  ||        | | |  ||         | | ||  ||         |  |
| |  || (meter | |  || +fader)| |  ||        | |  ||        | |  ||        | | |  ||         | | ||  ||         |  |
|  -3.2 dB     |  -1.0 dB     |  -8.5 dB     |  -2.0 dB     |  -6.0 dB     | |  -4.0 dB      | |  0.0 dB        |  |
| [Mute][Solo] | [Mute][Solo] | [Mute][Solo] | [Mute][Solo] | [Mute][Solo] | | [Mute][Solo]  | |                |  |
+--------------+--------------+--------------+--------------+--------------+ +---------------+ +----------------+  |
  (strips 96 px wide, scroll horizontally; master pinned right)           [+ Add Return]                          |
```

### 8.5 Agent approval banner (any view, 1920 width)

```
+------------------------------------------------------------------------------------------------------------------+
|[Sn][<-][->]                            ( Pattern )  Song   Mixer                          [Ag][In][Main Menu]    |
+------------------------------------------------------------------------------------------------------------------+
|[|<] [ > ] (o)Met | ...                                                                                           |
+------------------------------------------------------------------------------------------------------------------+
|  An agent wants to add the plugin Surge XT                                                            [ Review ] |
+------------------------------------------------------------------------------------------------------------------+
|  (content continues; the Agent page in the inspector opens on Review)                                            |
```

### 8.6 Start screen, 360x640

```
+--------------------------------------+
|              LibreDAW        [Menu]  |
+--------------------------------------+
| What do you want to make?            |
| Pick a style and press Play.         |
| You can change everything later.     |
|                                      |
| Start a Beat                         |
| +------------------+ +-------------+ |
| | [>] Phonk        | | [>] Trap    | |
| |  140 BPM         | |  140 BPM    | |
| +------------------+ +-------------+ |
| +------------------+ +-------------+ |
| | Boom Bap         | | Lo-fi       | |
| +------------------+ +-------------+ |
| +------------------+ +-------------+ |
| | House            | | Empty       | |
| +------------------+ +-------------+ |
| Recent Projects                      |
| +----------------------------------+ |
| | Night Drive    2 h ago     140  > | |
| +----------------------------------+ |
| Demo Projects                        |
| ...                                  |
| [Open Project...]                    |
+--------------------------------------+
```

### 8.7 Pattern view (steps), 360x640

```
+--------------------------------------+
|[<-][->]   Night Drive   [Sn] [Menu]  |   header (undo, redo, project name, sounds, menu)
+--------------------------------------+
| [|<] [ > ]   012:3:2     [140 v]     |   transport (tempo opens popover)
+--------------------------------------+
| Pattern 1 v [+]        [Settings v]  |
+--------------------------------------+
|    1 . . . 2 . . . |  (scrolls ->)   |
|= (o) Kick  [...]   # . . . # . . .   |   header column 120 px, then cells 36x44
|= (o) Snare [...]   . . . . # . . .   |
|= (o) Hat   [...]   # . # . # . # .   |
|= (o) 808   [...]   # . . . . . # .   |
| [+ Add Channel]                      |
|                                      |
+--------------------------------------+
| (o)Pattern   Song   Mixer            |   AdwViewSwitcherBar
+--------------------------------------+
```
`[...]` is the Channel Menu button ("Edit Sound", "Edit Notes", ...). Choosing "Edit Notes" switches to 8.8; "Edit Sound" opens the inspector as a full-width overlay.

### 8.8 Notes (piano roll), 360x640

```
+--------------------------------------+
|[< Steps]   Kick - Notes      [Menu]  |   back replaces undo/redo while in this sub-state
+--------------------------------------+
| [|<] [ > ]   012:3:2     [140 v]     |
+--------------------------------------+
| [Snap 1/16 v] [Key] [-][+]           |
|  C5 | .   ####                       |
|  B4 |                                |
|  A4 |        ####                    |
|  G4 |                                |
|  F4 | ####                           |
|  E4 |                                |
|  D4 |                                |
|  C4 |            |>                  |
| vel | | | |   |                      |
+--------------------------------------+
| (o)Pattern   Song   Mixer            |
+--------------------------------------+
```

### 8.9 Song view, 360x640

```
+--------------------------------------+
|[<-][->]   Night Drive   [Sn] [Menu]  |
+--------------------------------------+
| [|<] [ > ]   012:3:2     [140 v]     |
+--------------------------------------+
| Brush: Pattern 1 v   Snap: Bar v     |
+--------------------------------------+
| Drums | 1     5     9     13 ->      |   track name column 72 px (name, tap for Mute and Solo)
| Drums | [Pat1][Pat1][Pat2]           |
| Bass  |       [Pat3====][Pat3==]     |
| Melody|             [Pat4:.:.]       |
| [+ Add Track]                        |
+--------------------------------------+
|  Pattern  (o)Song   Mixer            |
+--------------------------------------+
```
At 360 px the track header column shows only color bar and a 2-line truncated name; a tap on the name opens a popover with "Rename", "Mute", "Solo".

### 8.10 Mixer, 360x640

```
+--------------------------------------+
|[<-][->]   Night Drive   [Sn] [Menu]  |
+--------------------------------------+
| [|<] [ > ]   012:3:2     [140 v]     |
+--------------------------------------+
| Kick       | Snare      | Master     |   strips 88 px; two scroll, Master pinned at right
| [Saturator]| [EQ]       | Limiter    |
| > Sends    | > Sends    |            |
| L--|--R    | L--|--R    | L--|--R    |
| ||  -6     | ||         | ||         |
| ||         | ||         | ||         |
| ||         | ||         | ||         |
| -3.2 dB    | -1.0 dB    | 0.0 dB     |
| [M][S]     | [M][S]     |            |
+--------------------------------------+
| Pattern   Song  (o)Mixer             |
+--------------------------------------+
```
At 360 px the strip buttons show "Mute" and "Solo" if 40 px fits; otherwise the toggles use icons with the full accessible name (the label is chosen by measured width, not by guess).

### 8.11 Sounds overlay and Inspector overlay at 360 px

```
 Sounds (F9 or [Sn])                         Inspector (from "Edit Sound")
+--------------------------------------+    +--------------------------------------+
| [x] Sounds                           |    | [x] Kick                             |
| Search sounds                        |    | [<] Dark Bell [>]       [Vary]       |
| All Drums Bass Melody ->             |    |  (o)   (o)   (o)   (o)               |
| (>) Kick Hard   Drums - Phonk        |    | Tone  Punch Grit  Length             |
| (>) 808 Long    Bass - Phonk         |    |  (o)   (o)   (o)   (o)               |
| ...                                  |    | Space Wobble Width Bright            |
| [Add Sound Folder...]                |    | v More Controls                      |
+--------------------------------------+    | [Expert Window]                      |
                                            | [Sound][Agent][History]              |
                                            +--------------------------------------+
```
Both overlays are `AdwOverlaySplitView` in `collapsed` mode; Esc and the scrim click close them.

---

## 9. Implementation order (one developer)

Principle: the app must look finished at every step. The shell is built first with real widgets; each step replaces one `AdwStatusPage` stand-in with the real view, so no step shows an empty or broken-looking window. All steps use the final widget tree names from section 2.1 so nothing is renamed later.

| Step | Build | Milestone | Looks coherent because |
|---|---|---|---|
| 1 | App shell: `AdwApplication`, `AdwApplicationWindow` with `AdwToastOverlay`, `AdwToolbarView`, header bar (menu, empty switcher stack), transport bar (Play/Stop, position, tempo, metronome wired to the engine), `style.css` loader, `Palette`, `SizeClass`, view state file, primary menu with About and Keyboard Shortcuts, Preferences with Audio page | A (Phase 1 metronome acceptance) | A real GNOME-looking window that plays a metronome; each page is an `AdwStatusPage` with a title and description, not a blank area |
| 2 | Breakpoints and adaptive skeleton: `bp-regular`, `bp-compact`, `bp-narrow`, switcher bar, overlay split views (empty sidebars show status pages), touch class | A | Resizing from 1920 to 360 already behaves; all later content inherits it |
| 3 | Steps section: `LdawStepGrid`, channel header list, lanes (velocity first), pattern strip, empty state, Add Channel with "Synth" | A, then B (lanes, swing) | A beat can be made and heard; empty state teaches the first click |
| 4 | Notes section: `LdawPianoRoll` on top of existing `view_math.rs` and `roll_logic.rs`, keyboard column, ruler, velocity lane, snap, zoom, keyboard path, accessibility | A | The Pattern page is complete (paned, focus modes) |
| 5 | Mixer: strips, fader, pan, mute, solo, `LdawMeter`, master | A (item 6 of 13.1) | Meter and faders complete the "real DAW" feel early |
| 6 | Undo/redo header buttons, toasts with Undo, error toast catalogue, History page (undo history first, named versions later) | A | Satisfies "undo works for everything" (SPEC 15.8.5) |
| 7 | Inspector "Sound" page for the built-in synth: `LdawKnob`, MacroGrid (native parameters first, `ParamTable`), "More Controls", "Expert Window" for the CLAP plugin | A (item 5, 8) | One consistent place for sound edits |
| 8 | Accessibility and theme pass: roles and value text, focus rings, high contrast, dark, 200 percent text scale, keyboard-only walk-through; screenshot tests at three sizes in light, dark, and high contrast | A gate | Catch palette and focus mistakes before more widgets exist |
| 9 | Start screen: templates (one built-in default first), recent, demos, `AdwNavigationView` root, 2-click path test | B (minimal), C (full) | First impression is final; the 2-click test (SPEC 15.8.1) becomes automatable |
| 10 | Sound browser: search, role filters, audition with debounce, drag onto Add Channel and channel headers, empty states, sound folders | C (item 4) | Drag and drop works against the already-final grid |
| 11 | Effects UI: insert slots, effect knobs in the Inspector, sends and returns, sidechain choice | B (items 5, 6) | Reuses `LdawKnob` and the strip layout |
| 12 | Song view: `LdawSongCanvas`, tracks, clips, mini previews, Pattern and Song mode toggle, export popover | C (items 1, 3) | A full song in the same visual language |
| 13 | Agent banner and Agent page, approval rows, activity list, `needs-attention` badges | A (17.1 trust model, can be earlier if the MCP milestone needs it) | Uses `AdwBanner` and boxed lists; no custom widgets |
| 14 | Macros and presets UI polish: preset prev/next, Vary, key and scale lock, level-matched audition indicators | D | Builds on steps 7 and 10 |
| 15 | Polish: icons (own SVGs in `ASSETS.md`), animations (playhead, flash), reduced motion, screenshot gallery for the README | any | Last because the stock stand-in icons already look right |

Notes for the developer:

- Build the two cheap debugging aids in step 1: an environment variable `LIBREDAW_SIZE=360x640` that sets the window's default size, and a hidden `--theme light|dark|hc` switch mapping to `AdwStyleManager::set_color_scheme` (plus `GTK_THEME=Adwaita:hc` for HC), so screenshots at each size and theme are one command.
- `AdwBreakpoint` conditions and setters are verified by a small GTK test (create the window, resize with `set_default_size`, read the setters' effects). Run it under a headless compositor (`weston --backend=headless` or a virtual Wayland session) in CI if available; if not, run it locally in the `tools/dev.fish` loop and treat it as a manual gate.
- Keep all sizes in a `tokens.rs` module (row heights, widths, gaps) with the pointer and touch variants side by side, so `bp-narrow` and `touch` changes are one place.
- Write `shortcuts.rs` (key, action, label, tooltip text) in step 1 and generate accelerators, tooltips, and the shortcuts window from it.
- Each custom widget lands with its logic tests first (as `roll_logic` already does), then the thin `snapshot()` wrapper, then the accessibility strings.
- Open decisions for the owner are in section 0; answer them before step 1 (the transport bar placement and the Space rule change the shell and every editor's key map).
