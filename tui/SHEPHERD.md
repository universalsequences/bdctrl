# Workflow shepherd

A persistent Fable session, one per epic workflow, that decides what the epic
should work on next. Today `advanceWorkflow` picks mechanically: ready beads
(`bd ready` — dependency edges are the only gate), sorted by priority then age,
top-N into free concurrency slots (`src/workflow.ts`). For large epics the
dependency graph is sparse and that degrades to "oldest highest-priority bead,
whatever that happens to be." The shepherd adds the missing judgment without
changing the picker.

## Locked decisions

1. **The shepherd steers through bd, not through the picker.** It expresses
   every decision as bd mutations — `bd update --priority`, dependency edges,
   splitting/creating beads — and the existing `advanceWorkflow` sort obeys.
   The picker stays dumb, deterministic, and testable; a dead shepherd degrades
   to today's behavior instead of stalling the epic. If a bead absolutely must
   be next, it simply becomes the highest-priority ready bead.
2. **One persistent session per epic, living in a Herdr tab** — the designer
   pattern (`spawnDesigner` in `src/herdr.ts`), binding ID `shepherd:<epicId>`,
   tab label `shep-<epicId>`. The session accumulates context about how the
   epic is going across consults; the harness's auto-compaction bounds context
   growth. The tab doubles as the user's steering console: focus it, argue with
   the shepherd, and the next automated consult inherits that conversation.
3. **Consults are event-driven, delivered in place.** When a workflow-launched
   bead closes, the viewer pings the existing session with
   `herdr agent prompt shep-<epicId> "<event>"` — no new tab, no new session.
   Only when no live shepherd agent exists (first consult, Herdr restarted, tab
   closed by hand) does the viewer create a tab and start Claude with
   `--resume <sessionId>`, so even process death keeps the conversation.
4. **Hold/release rides bd.** Before pinging, the viewer adds a
   `shepherd-hold` label to the epic bead. While the label is present the
   workflow launches nothing new (running beads are untouched — the hold
   occupies the decision point, not the machines). The shepherd's final act
   each consult is `bd update <epicId> --remove-label shepherd-hold`; the
   viewer's normal 2-second bd poll observes the release. No new IPC channel,
   and the hold survives viewer restarts because it lives in the database.
5. **Shepherd memory also lives in bd.** Each consult, the shepherd appends its
   current read — what's prioritized and why — to the epic's notes. Not its
   primary memory (the session is), but the audit trail, the re-grounding
   source after compaction or a session restart, and the human-readable
   "why are priorities what they are."
6. **Every close triggers a consult**, with a no-op fast path in the prompt:
   "if current priorities are still right, remove the hold and stop." One
   Fable turn per bead is the accepted cost — it is exactly the oversight the
   feature exists to buy.

## State

`Workflow` gains:

```ts
shepherd?: {
  model?: string          // Fable by default
  sessionId?: string      // captured after first spawn; used for --resume
  consulted: string[]     // bead IDs whose close has already been consulted
  consultingSince?: number // set when the hold is placed, cleared on release
}
```

The agent binding (`shepherd:<epicId>` → agent name) goes through the existing
`agents.json` machinery; `discoverAgentBindings` recovers a live shepherd by
name prefix after a viewer restart, exactly as it does for the designer.

## Consult lifecycle

1. `advanceWorkflow` notices a launched bead newly closed (already computed —
   the `closed(launch.id)` check) with `shepherd` configured and the bead not
   in `consulted`. It returns a `consult` action instead of refilling the slot.
2. The viewer adds `shepherd-hold` to the epic, records the bead in
   `consulted`, sets `consultingSince`, and delivers the consult prompt:
   - live shepherd agent → `herdr agent prompt <name> <prompt>`
   - none alive → create tab (`--no-focus`), start Claude with
     `--resume <sessionId>` when one is stored, send the intro + consult
     prompt, store the new session ID.
3. While the epic carries `shepherd-hold`, `advanceWorkflow` returns no
   launches (reviews still launch — the review stage owns its beads and review
   outcomes are input to the *next* consult, not gated by this one).
4. The shepherd inspects (`bd show`, `bd ready`, epic subtree), mutates
   priorities/deps/beads as needed, appends rationale to the epic notes, and
   removes the label.
5. Next poll: label gone → `consultingSince` cleared → slots refill under the
   possibly-rewritten priorities. The shepherd's decision takes effect with no
   further signaling.

**Workflow start:** when a workflow is created with a shepherd, the viewer
spawns the session immediately with an intro prompt (role, epic ID, the hold
protocol) plus an initial consult — the hold is placed before the first
launches, so initial priorities are vetted the same way subsequent ones are.

## Prompts

Intro (first spawn only): you are the shepherd for epic `<epicId>` in this
repository; your job is deciding what the workflow works on next, expressed
only through `bd` — priorities, dependency edges, creating or splitting beads.
You never implement beads and never claim or close them. Keep a running read of
the epic in its notes. When you finish a reassessment, run
`bd update <epicId> --remove-label shepherd-hold` — the workflow is paused
until you do.

Consult (every event): bead `<id>` (`<title>`) just closed. Reassess: look at
what it changed, what it unblocked, and any beads its implementer created —
including whether something higher-level is now missing. Adjust priorities and
dependencies so the right bead is next; append your updated read to the epic
notes. If current priorities are still right, say so briefly and just remove
the hold.

## Failure modes

- **Consult timeout** (default 10 minutes from `consultingSince`): the viewer
  removes the hold itself and continues under existing priorities — graceful
  degradation to today's behavior — surfacing a transient status. The session
  is left alone; a wedged shepherd is a tab the user can inspect.
- **Spawn failure**: pause the workflow visibly, same as worker launch
  failures. Resume retries the consult.
- **Label removed by hand**: indistinguishable from a completed consult, and
  that's fine — it is the manual override.
- **Viewer restart mid-consult**: the label and `consultingSince` persist
  (state file + bd), discovery re-finds the live agent, and the timeout math
  still holds.

## UI

Kept plain, per the viewer's style: the workflow modal gains a shepherd toggle
(with model choice) alongside review and concurrency; the workflow line shows
`· consulting shepherd` while the hold is present; `g` on the workflow (or a
modal entry) focuses the shepherd tab.

## Open questions

- Whether Herdr's `interactive_ready` toggles false while an agent is
  mid-turn. If it does, it's a free busy/idle signal and the label becomes the
  restart-safe backstop rather than the primary release; the design does not
  depend on it.
- Consult batching under concurrency > 1: two beads closing in one poll tick
  should produce one consult naming both, not two queued consults. (Covered by
  checking `consulted` set membership per tick and joining the events.)
