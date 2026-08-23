# beadsviewer

A small [OpenTUI](https://opentui.com/) viewer/launcher for Beads and Herdr.

## Run

Requires Bun, `bd`, and (for launching agents) a Herdr-managed pane.

```sh
cd tui
bun install
bun start -- /path/to/beads/project
```

The path defaults to the current directory. Browsing works outside Herdr; launching an agent does not.

## Controls

- Click a bead to select it and focus its list
- Scroll the mouse wheel over either list to move its selection
- `Tab`: switch between the ready and in-progress lists
- `↑` / `↓`: select a bead (newest is first)
- `e`: toggle ready beads / open epics
- `p`: launch Pi for the selected ready bead
- `c`: launch Claude Code for the selected ready bead
- `Q`: queue the selected ready bead (press again to unqueue)
- `w`: open the epic workflow modal for the selected epic (or the epic a selected bead belongs to)
- `d`: jump to the bead designer, launching one (Claude Code) if none is running
- `g`: jump to the Herdr agent for a selected in-progress bead launched by this viewer
- `r`: refresh
- `q` or `Ctrl-C`: quit

An **In progress** pane sits below the ready list. The viewer polls Beads every two seconds without moving your cursor, and optimistically moves a bead there as soon as you launch an agent rather than waiting for the agent's claim command. A `◆` marks beads tied to a live Herdr agent. These associations are saved under `$XDG_STATE_HOME/beadsviewer/` (or `~/.local/state/beadsviewer/`) and restored after restarting the viewer.

## Queue

`Q` queues the selected ready bead instead of launching it now, and you can queue as many as you like. The queue drains strictly one at a time, and only when **no** agent is working — meaning no live Herdr agent attached to a bead that is *still in progress*, and no launch in flight. In-progress beads with no live agent (the ones you lost track of) do not hold the queue back, and neither does an agent whose tab is still open after its bead closed. The **In progress** title says what the queue is waiting on: `5 queued · waiting on eseq-u2h`. Queued beads stay in the ready list marked `⋯ 1`, `⋯ 2`, … in launch order, with the count in the **In progress** pane title; `Q` again removes one, and launching a queued bead with `p`/`c` drops it from the queue. Queued beads use the Claude harness (same as `c`), and the queue persists in the same state directory as workflows.

A queued bead that gets closed or claimed by someone else is dropped; if its agent fails to start, it goes back to the front of the queue and is retried.

## Epic workflows

`w` starts a workflow that works through a whole epic automatically: pick a harness (Claude · Fable, Claude · Opus 5, or Pi), a concurrency (sequential, or 2–3 beads at a time sharing the working tree), where to work (your current branch, or a dedicated worktree), and an optional review step, and the viewer launches agents for the epic's ready beads, waits for each bead to close, and launches whatever that unblocks — `bd ready` decides what is launchable, so diamond-shaped dependency graphs just work. Active workflows appear in an **Epic workflows** pane below the in-progress list with progress (`3/7 closed · 1 running · 1 in review`); click a workflow there (or press `w` on its epic) to resume it, change its review step or concurrency, or cancel it.

Choosing the worktree option creates a git worktree at a sibling directory (`../<repo>-<epic-id>`) on a `workflow/<epic-id>` branch — via `bd worktree create`, so the worktree shares the same beads database as the main checkout (plain `git worktree add` is the fallback for older bd). Everything the workflow launches (workers and reviewers) runs inside that worktree, and its beads stop counting against the "no agent is working" gate: the queue keeps draining and you can launch other beads on your branch in parallel, while the workflow's own concurrency setting still applies within its worktree. The workflow line shows the branch (`⎇ workflow/epic-1`). When the workflow finishes or is cancelled, the worktree and branch are left in place for you to merge; restarting a workflow for the same epic re-attaches to the existing worktree.

With a review step, workers do not close their beads: they commit, add a `needs-review` label, and leave the bead in progress. The workflow then launches the chosen reviewer harness (e.g. Claude · Fable) with a prompt to verify the commit against the bead's description and acceptance criteria, fix any problems itself, then remove the label and close the bead — so downstream beads stay blocked until review passes, and the loop always converges. Changing a running workflow's review step only affects beads launched afterwards; agents already working were prompted with the previous completion instructions.

A workflow pauses — rather than looping or silently stopping — when a launched agent disappears without closing its bead, or when open beads remain but none are ready. Press `w` on the epic to resume (stalled beads are relaunched) or cancel; cancelling never touches agents that are already running. Workflows persist in the same state directory, so restarting the viewer resumes them.

Every launched agent gets its own tab (labeled with the bead ID) — panes are never split. The launched agent is focused and receives a prompt to inspect and atomically claim the selected bead. The viewer remains running in its original pane.

## Development

```sh
bun test
bun run check
```
