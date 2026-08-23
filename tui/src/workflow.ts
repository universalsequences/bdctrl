import type { Bead } from "./beads"
import { runCommand, runCommandResult } from "./beads"
import { readStateArray, stateFilePath, stateRoot, type AgentKind } from "./herdr"
import { mkdir } from "node:fs/promises"
import { basename, dirname, join, resolve } from "node:path"

export type WorkflowLaunch = { id: string, at: number }
export type ReviewConfig = { kind: AgentKind, model?: string }
export type WorktreeConfig = { path: string, branch: string }

export type Workflow = {
  cwd: string
  epicId: string
  kind: AgentKind
  model?: string
  concurrency: number
  launched: WorkflowLaunch[]
  review?: ReviewConfig
  reviews?: WorkflowLaunch[]
  worktree?: WorktreeConfig
  paused?: string
}

export type WorkflowAdvance = {
  // null means every child bead is closed and the workflow is finished.
  workflow: Workflow | null
  launches: Bead[]
  reviews: Bead[]
}

// Workers in a reviewed workflow add this label instead of closing their bead;
// the reviewer removes it when it closes the bead.
export const REVIEW_LABEL = "needs-review"

// launchedAgents key for a bead's reviewer, distinct from its worker.
export function reviewBindingId(beadId: string): string {
  return `review:${beadId}`
}

// A freshly spawned agent takes a while to claim its bead and show up in
// `herdr agent list`, so a launch is only considered stalled after this grace.
export const STALL_GRACE_MS = 90_000

// Deterministic worktree naming: the same epic always maps to the same branch
// and sibling directory, so a restarted viewer re-attaches instead of creating
// a second worktree.
export function worktreeDefaults(cwd: string, epicId: string): WorktreeConfig {
  const project = resolve(cwd)
  const slug = epicId.toLowerCase().replace(/[^a-z0-9._-]/g, "-")
  return {
    branch: `workflow/${slug}`,
    path: join(dirname(project), `${basename(project)}-${slug}`),
  }
}

// Beads launched by a worktree'd workflow never touch the main working tree,
// so they do not count against the "one agent at a time" gate that protects
// the user's branch. Their review bindings map back to these same bead IDs.
export function worktreeExemptIds(workflows: Workflow[]): Set<string> {
  const ids = new Set<string>()
  for (const workflow of workflows) {
    if (!workflow.worktree) continue
    for (const launch of workflow.launched) ids.add(launch.id)
  }
  return ids
}

// Create the workflow's worktree, or quietly reuse one that already exists on
// the expected branch (e.g. the viewer restarted mid-workflow). `bd worktree
// create` shares the beads database with the main checkout via the git common
// directory; older bd falls back to plain git, which shares it the same way.
export async function ensureWorktree(cwd: string, config: WorktreeConfig): Promise<void> {
  const path = resolve(config.path)
  const listed = await runCommand(["git", "worktree", "list", "--porcelain"], cwd)
  for (const block of listed.split("\n\n")) {
    const lines = block.split("\n")
    const existing = lines.find((line) => line.startsWith("worktree "))?.slice("worktree ".length)
    if (!existing || resolve(existing) !== path) continue
    const branch = lines.find((line) => line.startsWith("branch "))?.slice("branch refs/heads/".length)
    if (branch === config.branch) return
    throw new Error(`${path} already exists on branch ${branch ?? "(detached)"}`)
  }

  const created = await runCommandResult(["bd", "worktree", "create", path, "--branch", config.branch], cwd)
  if (created.exitCode === 0) return
  const branchExists = (await runCommandResult(
    ["git", "rev-parse", "--verify", "--quiet", `refs/heads/${config.branch}`], cwd,
  )).exitCode === 0
  await runCommand(
    ["git", "worktree", "add", ...(branchExists ? [path, config.branch] : ["-b", config.branch, path])], cwd,
  )
}

export async function loadWorkflows(cwd: string): Promise<Workflow[]> {
  const project = resolve(cwd)
  const scoped = await readStateArray<Workflow>(stateFilePath(cwd, "workflows.json"))
  if (scoped) return scoped.filter((workflow) => workflow.cwd === project)
  // Legacy shared file from before per-project state; the first save migrates.
  const legacy = await readStateArray<Workflow>(join(stateRoot, "workflows.json"))
  return (legacy ?? []).filter((workflow) => workflow.cwd === project)
}

export async function saveWorkflows(cwd: string, workflows: Workflow[]): Promise<void> {
  const path = stateFilePath(cwd, "workflows.json")
  await mkdir(dirname(path), { recursive: true })
  await Bun.write(path, JSON.stringify(workflows, null, 2) + "\n")
}

function needsReview(child: Bead | undefined): boolean {
  return Boolean(child?.labels?.includes(REVIEW_LABEL))
}

export function advanceWorkflow(
  workflow: Workflow,
  children: Bead[],
  readyIds: Set<string>,
  agentAlive: (bindingId: string) => boolean,
  now = Date.now(),
): WorkflowAdvance {
  // The issue graph may not include children yet (old bd, mid-load); treat the
  // workflow as waiting rather than complete or stuck.
  if (children.length === 0) return { workflow, launches: [], reviews: [] }

  const byId = new Map(children.map((child) => [child.id, child]))
  const closed = (id: string) => byId.get(id)?.status === "closed"
  const remaining = children.filter((child) => child.status !== "closed").length
  if (remaining === 0) return { workflow: null, launches: [], reviews: [] }
  if (workflow.paused) return { workflow, launches: [], reviews: [] }

  const pending = workflow.launched.filter((launch) => !closed(launch.id))
  const pendingReviews = (workflow.reviews ?? []).filter((launch) => !closed(launch.id))
  const reviewedIds = new Set((workflow.reviews ?? []).map((launch) => launch.id))

  // A worker that labeled its bead needs-review is done with it: the bead now
  // belongs to the review stage, so a gone worker is not a stall.
  const awaitingReview = (id: string) => Boolean(workflow.review) && needsReview(byId.get(id))
  const stalledWorkers = pending.filter((launch) =>
    !awaitingReview(launch.id) && !agentAlive(launch.id) && now - launch.at > STALL_GRACE_MS)
  const stalledReviews = pendingReviews.filter((launch) =>
    !agentAlive(reviewBindingId(launch.id)) && now - launch.at > STALL_GRACE_MS)
  if (stalledWorkers.length > 0 || stalledReviews.length > 0) {
    const parts = [
      ...stalledWorkers.map((launch) => `agent for ${launch.id}`),
      ...stalledReviews.map((launch) => `reviewer for ${launch.id}`),
    ]
    return {
      workflow: { ...workflow, paused: `${parts.join(", ")} exited without closing the bead` },
      launches: [], reviews: [],
    }
  }

  const reviews = workflow.review
    ? pending
        .map((launch) => byId.get(launch.id))
        .filter((child): child is Bead => needsReview(child) && !reviewedIds.has(child!.id))
    : []

  const launchedIds = new Set(workflow.launched.map((launch) => launch.id))
  const candidates = children
    .filter((child) => readyIds.has(child.id) && !launchedIds.has(child.id))
    .sort((a, b) => a.priority - b.priority || (a.created_at ?? "").localeCompare(b.created_at ?? ""))
  const launches = candidates.slice(0, Math.max(0, workflow.concurrency - pending.length))

  // In-progress children this workflow did not launch (e.g. claimed manually)
  // still count as movement — wait for them instead of declaring a dead end.
  const moving = children.some((child) => child.status === "in_progress")
  if (launches.length === 0 && pending.length === 0 && reviews.length === 0 && !moving) {
    const noun = remaining === 1 ? "bead" : "beads"
    return { workflow: { ...workflow, paused: `${remaining} ${noun} still open but none ready` }, launches: [], reviews: [] }
  }
  return { workflow, launches, reviews }
}

export function resumeWorkflow(
  workflow: Workflow,
  children: Bead[],
  agentAlive: (bindingId: string) => boolean,
): Workflow {
  const byId = new Map(children.map((child) => [child.id, child]))
  const closed = (id: string) => byId.get(id)?.status === "closed"
  // Drop stalled launches so their beads are eligible to be launched again. A
  // labeled bead keeps its worker entry — the review stage owns it now.
  const launched = workflow.launched.filter((launch) =>
    closed(launch.id) || agentAlive(launch.id) || (Boolean(workflow.review) && needsReview(byId.get(launch.id))))
  const reviews = (workflow.reviews ?? []).filter((launch) =>
    closed(launch.id) || agentAlive(reviewBindingId(launch.id)))
  const resumed: Workflow = { ...workflow, launched, reviews }
  delete resumed.paused
  return resumed
}
