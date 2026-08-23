import type { Bead } from "./beads"
import { readStateArray, stateFilePath, type AgentKind } from "./herdr"
import { mkdir } from "node:fs/promises"
import { dirname, resolve } from "node:path"

export type QueueEntry = {
  cwd: string
  id: string
  kind: AgentKind
  model?: string
  at: number
}

export async function loadQueue(cwd: string): Promise<QueueEntry[]> {
  const project = resolve(cwd)
  const entries = await readStateArray<QueueEntry>(stateFilePath(cwd, "queue.json"))
  return (entries ?? []).filter((entry) => entry.cwd === project && typeof entry.id === "string")
}

export async function saveQueue(cwd: string, queue: QueueEntry[]): Promise<void> {
  const path = stateFilePath(cwd, "queue.json")
  await mkdir(dirname(path), { recursive: true })
  await Bun.write(path, JSON.stringify(queue, null, 2) + "\n")
}

// A queued bead that somebody claimed or closed in the meantime is no longer
// ours to launch. Unknown IDs are kept: the issue graph may not be loaded yet.
export function pruneQueue(queue: QueueEntry[], issuesById: Map<string, Bead>): QueueEntry[] {
  return queue.filter((entry) => {
    const bead = issuesById.get(entry.id)
    return !bead || (bead.status !== "closed" && bead.status !== "in_progress")
  })
}

// launchedAgents is keyed by bead ID for workers and `review:<id>` for
// reviewers; the designer has its own sentinel key.
export function beadIdForBinding(binding: string): string {
  return binding.startsWith("review:") ? binding.slice("review:".length) : binding
}

// What is genuinely being worked on right now. An agent whose bead is no
// longer in progress does not count: agents outlive their bead (the tab stays
// open after the bead is closed), and an in-progress bead with no live agent
// is one the user lost track of, not work the queue should wait behind.
// `exempt` beads (worktree'd workflows) run in their own working tree and
// never block work on the user's branch.
export function blockingBeadIds(options: {
  bindings: Iterable<string>
  inProgressIds: Iterable<string>
  launching: Iterable<string>
  exempt?: Iterable<string>
}): Set<string> {
  const inProgress = new Set(options.inProgressIds)
  const exempt = new Set(options.exempt ?? [])
  const blocking = new Set<string>()
  for (const id of options.launching) {
    const beadId = beadIdForBinding(id)
    if (!exempt.has(beadId)) blocking.add(beadId)
  }
  for (const binding of options.bindings) {
    const beadId = beadIdForBinding(binding)
    if (inProgress.has(beadId) && !exempt.has(beadId)) blocking.add(beadId)
  }
  return blocking
}

// The whole point of the queue: nothing starts while an agent is still
// working, so queued beads never share the working tree with another agent.
export function nextQueued(
  queue: QueueEntry[],
  options: { readyIds: Set<string>, activeAgents: number },
): QueueEntry | undefined {
  if (options.activeAgents > 0) return undefined
  return queue.find((entry) => options.readyIds.has(entry.id))
}
