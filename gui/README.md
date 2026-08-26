# beadsctrl GUI

A minimal GPUI dashboard for seeing and organizing Beads epics. The existing TUI remains the command center; this app is the bird's-eye view.

## Run

Requirements: Rust, `bd` on `PATH`, and GPUI's Linux system dependencies.

```sh
cd gui
cargo run -- ../../eseq
```

With no path, the current directory is used.

## Current scope

- Epic cards with progress and active child beads
- Ready, blocked, in-progress, and closed totals
- Click any bead or epic to inspect it
- Click a `P0`–`P4` pill to advance its priority
- Move a bead between epics from the inspector
- Launch Pi or Claude agents through Herdr from the inspector
- Hover working beads for a live agent-output preview; use `•••` to focus the agent
- Agent tabs started by hand join the deck too: an untracked Herdr pane is
  matched to the bead it claimed, from its Claude transcript or its terminal
  output, and marked `external`
- Automatic refresh after external changes, with completion notifications
- `Ctrl+F` search across epic titles, bead titles, and bead IDs

All reads and writes go through the `bd` CLI. No Beads database files are accessed directly.
