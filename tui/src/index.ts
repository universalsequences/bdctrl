import {
  BoxRenderable,
  createCliRenderer,
  SelectRenderable,
  SelectRenderableEvents,
  StyledText,
  TextRenderable,
  bold,
  fg,
  type KeyEvent,
  type TextChunk,
} from "@opentui/core"
import { basename, resolve } from "node:path"
import { age, loadBeads, loadInProgress, loadIssueGraph, matchesBead, type Bead, type View } from "./beads"
import { DESIGNER_ID, discoverAgentBindings, focusAgent, spawnAgent, spawnDesigner, spawnReviewer, type AgentKind } from "./herdr"
import { advanceWorkflow, ensureWorktree, loadWorkflows, resumeWorkflow, reviewBindingId, saveWorkflows, worktreeDefaults, worktreeExemptIds, type ReviewConfig, type Workflow, type WorktreeConfig } from "./workflow"
import { blockingBeadIds, loadQueue, nextQueued, pruneQueue, saveQueue, type QueueEntry } from "./queue"

const cwd = resolve(process.argv[2] ?? process.cwd())
const repoName = basename(cwd)
const renderer = await createCliRenderer({ exitOnCtrlC: false, useMouse: true })
renderer.setTerminalTitle("beadsctrl")

let view: View = "ready"
let beads: Bead[] = []
let inProgress: Bead[] = []
let issuesByID = new Map<string, Bead>()
let epicByChild = new Map<string, Bead>()
let childrenByEpic = new Map<string, Bead[]>()
let blockedBy = new Map<string, string[]>()
let workflows: Workflow[] = []
let workflowReady: Bead[] = []
const workflowLaunching = new Set<string>()
let queue: QueueEntry[] = []
const queueLaunching = new Set<string>()
let modalState: { epic: Bead, kind?: AgentKind, model?: string, concurrency?: number, worktree?: WorktreeConfig, edit?: boolean } | null = null
let refreshing = false
let launching = false
let agentRevision = 0
let statusRevision = 0
let activeList: "ready" | "progress" = "ready"
let searchQuery = ""
let searchInput = false
const optimisticClaims = new Map<string, { bead: Bead }>()
const launchedAgents = new Map<string, string>()

const app = new BoxRenderable(renderer, {
  id: "app", width: "100%", height: "100%", flexDirection: "column", backgroundColor: "#101419",
})
const header = new TextRenderable(renderer, {
  id: "header", height: 2, fg: "#7dd3fc", paddingX: 1, content: "beadsctrl",
})
const body = new BoxRenderable(renderer, {
  id: "body", width: "100%", flexGrow: 1, flexDirection: "row", gap: 1, paddingX: 1, overflow: "hidden",
})
const leftColumn = new BoxRenderable(renderer, {
  id: "left-column", width: "55%", height: "100%", flexGrow: 0, flexShrink: 0,
  flexDirection: "column", gap: 1, overflow: "hidden",
})
const listBox = new BoxRenderable(renderer, {
  id: "list-box", width: "100%", flexGrow: 1, flexBasis: 0, minHeight: 6, overflow: "hidden",
  border: true, borderColor: "#334155", titleColor: "#7dd3fc",
})
const list = new SelectRenderable(renderer, {
  id: "beads", width: "100%", height: "100%", padding: 1,
  options: [], wrapSelection: true, showDescription: true, showScrollIndicator: true,
  textColor: "#cbd5e1", descriptionColor: "#64748b",
  selectedBackgroundColor: "#164e63", selectedTextColor: "#f0f9ff",
  selectedDescriptionColor: "#bae6fd", focusedBackgroundColor: "#101419",
})
const progressBox = new BoxRenderable(renderer, {
  id: "progress-box", width: "100%", height: "35%", flexGrow: 0, flexShrink: 0, overflow: "hidden",
  border: true, borderColor: "#334155", title: "In progress", titleColor: "#fbbf24",
})
const progressList = new SelectRenderable(renderer, {
  id: "in-progress", width: "100%", height: "100%", padding: 1,
  options: [], showDescription: true, showScrollIndicator: true, showSelectionIndicator: false,
  wrapSelection: true, textColor: "#e2e8f0", descriptionColor: "#a16207",
  selectedBackgroundColor: "#101419", selectedTextColor: "#e2e8f0",
  selectedDescriptionColor: "#a16207", focusedBackgroundColor: "#101419",
})
const workflowBox = new BoxRenderable(renderer, {
  id: "workflow-box", width: "100%", height: 0, flexGrow: 0, flexShrink: 0, overflow: "hidden",
  border: true, borderColor: "#334155", title: "Epic workflows", titleColor: "#c4b5fd", visible: false,
})
const workflowText = new TextRenderable(renderer, {
  id: "workflows", width: "100%", height: "100%", padding: 1, fg: "#cbd5e1", content: "",
})
const modalBox = new BoxRenderable(renderer, {
  id: "workflow-modal", position: "absolute", left: "20%", top: 4, width: "60%", height: 12,
  zIndex: 100, visible: false, flexDirection: "column", overflow: "hidden",
  border: true, borderColor: "#a78bfa", backgroundColor: "#171226",
  title: "Epic workflow", titleColor: "#c4b5fd",
})
const modalHeader = new TextRenderable(renderer, {
  id: "workflow-modal-header", width: "100%", height: 2, paddingX: 1, paddingTop: 1, fg: "#e9d5ff", content: "",
})
const modalSelect = new SelectRenderable(renderer, {
  id: "workflow-modal-select", width: "100%", flexGrow: 1, padding: 1,
  options: [], wrapSelection: true, showDescription: true, showScrollIndicator: false,
  textColor: "#e2e8f0", descriptionColor: "#7c7195", backgroundColor: "#171226",
  selectedBackgroundColor: "#4c1d95", selectedTextColor: "#faf5ff",
  selectedDescriptionColor: "#d8b4fe", focusedBackgroundColor: "#171226",
})
const detailBox = new BoxRenderable(renderer, {
  id: "detail-box", flexBasis: 0, flexGrow: 1, flexShrink: 1, minWidth: 0, height: "100%", overflow: "hidden",
  border: true, borderColor: "#334155", title: "Details", titleColor: "#7dd3fc",
})
const detail = new TextRenderable(renderer, {
  id: "detail", width: "100%", minWidth: 0, height: "100%", padding: 1,
  fg: "#cbd5e1", wrapMode: "word", content: "",
})
const footer = new TextRenderable(renderer, {
  id: "footer", height: 2, paddingX: 1, fg: "#94a3b8", content: "",
})

listBox.add(list)
progressBox.add(progressList)
workflowBox.add(workflowText)
leftColumn.add(listBox)
leftColumn.add(progressBox)
leftColumn.add(workflowBox)
detailBox.add(detail)
body.add(leftColumn)
body.add(detailBox)
app.add(header)
app.add(body)
app.add(footer)
modalBox.add(modalHeader)
modalBox.add(modalSelect)
renderer.root.add(app)
renderer.root.add(modalBox)

function selectedReady(): Bead | undefined {
  return list.getSelectedOption()?.value as Bead | undefined
}

function selectedProgress(): Bead | undefined {
  return progressList.getSelectedOption()?.value as Bead | undefined
}

function selected(): Bead | undefined {
  return activeList === "ready" ? selectedReady() : selectedProgress()
}

function focusList(target: "ready" | "progress"): void {
  activeList = target
  const readyActive = target === "ready"

  // SelectRenderable retains a selected index while blurred, so make the
  // inactive selection visually identical to an ordinary row. This leaves a
  // single cursor/highlight across both lists.
  list.showSelectionIndicator = readyActive
  list.selectedBackgroundColor = readyActive ? "#164e63" : "#101419"
  list.selectedTextColor = readyActive ? "#f0f9ff" : "#cbd5e1"
  list.selectedDescriptionColor = readyActive ? "#bae6fd" : "#64748b"
  progressList.showSelectionIndicator = !readyActive
  progressList.selectedBackgroundColor = readyActive ? "#101419" : "#713f12"
  progressList.selectedTextColor = readyActive ? "#e2e8f0" : "#fffbeb"
  progressList.selectedDescriptionColor = readyActive ? "#a16207" : "#fde68a"

  listBox.borderColor = readyActive ? "#38bdf8" : "#334155"
  progressBox.borderColor = readyActive ? "#334155" : "#f59e0b"
  if (readyActive) list.focus()
  else progressList.focus()
  updateDetail()
  updateChrome()
}

function updateTitles(): void {
  const countBadge = searchQuery ? `${list.options.length}/${beads.length} · /${searchQuery}` : `${beads.length}`
  listBox.title = view === "ready" ? `Ready · newest first · ${countBadge}` : `Open epics · newest first · ${countBadge}`
  const blocking = queue.length > 0 ? [...blockingBeads()] : []
  const queueState = queue.length === 0
    ? ""
    : blocking.length > 0
      ? ` · ${queue.length} queued · waiting on ${blocking.slice(0, 2).join(", ")}${blocking.length > 2 ? `, +${blocking.length - 2}` : ""}`
      : ` · ${queue.length} queued · starting`
  progressBox.title = `In progress · ${progressList.options.length}${queueState}`
  header.content = ` beadsctrl  ${repoName}`
}

function updateChrome(message = ""): number {
  const revision = ++statusRevision
  updateTitles()
  footer.content = message || (searchInput
    ? ` /${searchQuery}▌   type to filter   Enter keep filter   Esc clear   ↑/↓ select`
    : searchQuery
      ? ` /${searchQuery}   Esc clear filter   ↑/↓ select   p Pi   c Claude   Q queue   w workflow   g go to agent`
      : ` Tab switch list   ↑/↓ select   / search   p Pi   c Claude   Q queue   w workflow   d designer   g go to agent   e ${view === "ready" ? "epics" : "ready"}   r refresh   q quit`)
  return revision
}

function showTransientStatus(message: string, duration = 3_000): void {
  const revision = updateChrome(message)
  const timer = setTimeout(() => {
    if (statusRevision === revision) updateChrome()
  }, duration)
  timer.unref()
}

function rebuildIssueGraph(issues: Bead[]): void {
  issuesByID = new Map(issues.map((issue) => [issue.id, issue]))
  epicByChild = new Map()
  childrenByEpic = new Map()
  blockedBy = new Map()
  const parentByChild = new Map<string, string>()

  for (const issue of issues) {
    for (const dependency of issue.dependencies ?? []) {
      if (dependency.type === "parent-child") {
        parentByChild.set(dependency.issue_id, dependency.depends_on_id)
      } else if (dependency.type === "blocks") {
        const dependents = blockedBy.get(dependency.depends_on_id) ?? []
        if (!dependents.includes(dependency.issue_id)) dependents.push(dependency.issue_id)
        blockedBy.set(dependency.depends_on_id, dependents)
      }
    }
  }

  // A bead may be nested under another task, so walk all the way to its epic
  // rather than only labelling direct children.
  for (const issue of issues) {
    const seen = new Set<string>([issue.id])
    let parentID = parentByChild.get(issue.id)
    while (parentID && !seen.has(parentID)) {
      seen.add(parentID)
      const parent = issuesByID.get(parentID)
      if (parent?.issue_type === "epic") {
        epicByChild.set(issue.id, parent)
        const siblings = childrenByEpic.get(parent.id) ?? []
        siblings.push(issue)
        childrenByEpic.set(parent.id, siblings)
        break
      }
      parentID = parentByChild.get(parentID)
    }
  }
}

function option(bead: Bead) {
  const epic = epicByChild.get(bead.id)
  const epicBadge = epic ? `◈ EPIC ${epic.id} ${epic.title} · ` : ""
  const typeBadge = bead.issue_type === "epic" ? "◆ EPIC  " : ""
  return {
    name: `${typeBadge}${bead.id}  P${bead.priority}  ${bead.title}`,
    description: `${epicBadge}${bead.issue_type} · ${age(bead.created_at)}${bead.assignee ? ` · @${bead.assignee}` : ""}`,
    value: bead,
  }
}

function renderLists(previous = {
  ready: selectedReady()?.id,
  progress: selectedProgress()?.id,
}): void {
  for (const id of optimisticClaims.keys()) {
    if (inProgress.some((bead) => bead.id === id)) optimisticClaims.delete(id)
  }

  const pendingIds = new Set(optimisticClaims.keys())
  beads = beads.filter((bead) => !pendingIds.has(bead.id))
  const optimistic = [...optimisticClaims.values()]
    .map(({ bead }) => ({ ...bead, status: "in_progress" }))
    .filter((bead) => !inProgress.some((current) => current.id === bead.id))
  // Keep the bd result canonical. If optimistic rows were written back into
  // `inProgress`, the next render mistook its own optimistic row for a real
  // claim and removed it before the agent had time to run `bd update --claim`.
  const displayedInProgress = [...optimistic, ...inProgress]

  const matchesSearch = (bead: Bead) => matchesBead(bead, searchQuery, epicByChild.get(bead.id))
  const visibleReady = beads.filter(matchesSearch)
  const visibleProgress = displayedInProgress.filter(matchesSearch)

  const queuePositions = new Map(queue.map((entry, index) => [entry.id, index + 1]))
  list.options = visibleReady.map((bead) => {
    const item = option(bead)
    const position = queuePositions.get(bead.id)
    if (position === undefined) return item
    const entry = queue[position - 1]!
    item.name = `⋯ ${position}  ${item.name}`
    item.description += ` · queued #${position} · ${harnessLabel(entry.kind, entry.model)}`
    return item
  })
  progressList.options = visibleProgress.map((bead) => {
    const item = option(bead)
    if (launchedAgents.has(reviewBindingId(bead.id))) {
      item.name = `◆ ${item.name}`
      item.description += " · review agent · g to focus"
    } else if (launchedAgents.has(bead.id)) {
      item.name = `◆ ${item.name}`
      item.description += " · Herdr agent · g to focus"
    }
    return item
  })
  const readyIndex = previous.ready ? visibleReady.findIndex((bead) => bead.id === previous.ready) : -1
  if (readyIndex >= 0 && readyIndex !== list.getSelectedIndex()) list.setSelectedIndex(readyIndex)
  const progressIndex = previous.progress ? visibleProgress.findIndex((bead) => bead.id === previous.progress) : -1
  if (progressIndex >= 0 && progressIndex !== progressList.getSelectedIndex()) progressList.setSelectedIndex(progressIndex)
  // Filtering can shrink a list past the retained cursor position.
  if (readyIndex < 0 && list.getSelectedIndex() >= list.options.length) list.setSelectedIndex(0)
  if (progressIndex < 0 && progressList.getSelectedIndex() >= progressList.options.length) progressList.setSelectedIndex(0)
  updateDetail()
}

const detailColors = {
  text: "#cbd5e1",
  bright: "#f8fafc",
  muted: "#64748b",
  accent: "#38bdf8",
  epic: "#c4b5fd",
  priority: "#f472b6",
  open: "#94a3b8",
  progress: "#fbbf24",
  closed: "#4ade80",
  danger: "#fb7185",
} as const

function colored(text: string, color: string, strong = false): TextChunk {
  const chunk = fg(color)(text)
  return strong ? bold(chunk) : chunk
}

function statusStyle(status: string): { mark: string, color: string } {
  if (status === "closed") return { mark: "✓", color: detailColors.closed }
  if (status === "in_progress") return { mark: "◐", color: detailColors.progress }
  if (status === "open") return { mark: "○", color: detailColors.open }
  return { mark: "·", color: detailColors.muted }
}

function blocksTreeChunks(rootID: string): TextChunk[] {
  const rootChildren = blockedBy.get(rootID) ?? []
  if (rootChildren.length === 0) return [colored("└─ none", detailColors.muted)]

  const chunks: TextChunk[] = []
  let lineCount = 0
  const append = (id: string, prefix: string, last: boolean, ancestors: Set<string>): void => {
    if (lineCount++ > 0) chunks.push(colored("\n", detailColors.text))
    const issue = issuesByID.get(id)
    chunks.push(colored(`${prefix}${last ? "└─" : "├─"} `, detailColors.muted))
    if (issue) {
      const status = statusStyle(issue.status)
      chunks.push(colored(`${status.mark} `, status.color, true))
      chunks.push(colored(issue.id, detailColors.accent))
      chunks.push(colored(`  P${issue.priority}  `, detailColors.priority))
      chunks.push(colored(issue.title, issue.status === "closed" ? detailColors.muted : detailColors.text))
    } else {
      chunks.push(colored(`· ${id}`, detailColors.muted))
    }
    if (ancestors.has(id)) {
      chunks.push(colored("  ↺ cycle", detailColors.danger))
      return
    }

    const children = [...(blockedBy.get(id) ?? [])].sort((a, b) => {
      const left = issuesByID.get(a), right = issuesByID.get(b)
      return Number(left?.status === "closed") - Number(right?.status === "closed")
        || (left?.priority ?? 99) - (right?.priority ?? 99)
        || a.localeCompare(b)
    })
    const nextAncestors = new Set(ancestors).add(id)
    const nextPrefix = prefix + (last ? "   " : "│  ")
    children.forEach((child, index) => append(child, nextPrefix, index === children.length - 1, nextAncestors))
  }

  rootChildren.forEach((child, index) => append(child, "", index === rootChildren.length - 1, new Set([rootID])))
  return chunks
}

function updateDetail(bead = selected()): void {
  detailBox.title = activeList === "ready" ? "Details · ready" : "Details · in progress"
  if (!bead) {
    detail.content = refreshing ? "Loading…" : `No ${activeList === "ready" ? "beads in this view" : "in-progress beads"}.`
    return
  }
  const labels = bead.labels?.length ? bead.labels.join(", ") : "—"
  const epic = epicByChild.get(bead.id)
  const status = statusStyle(bead.status)
  const chunks: TextChunk[] = [
    colored(bead.title, detailColors.bright, true),
    colored("\n\nID ", detailColors.accent, true),
    colored(bead.id, detailColors.text),
    colored("   TYPE ", detailColors.accent, true),
    colored(bead.issue_type, bead.issue_type === "epic" ? detailColors.epic : detailColors.text),
    colored("   PRIORITY ", detailColors.accent, true),
    colored(`P${bead.priority}`, detailColors.priority),
    colored("\nAGE ", detailColors.accent, true),
    colored(age(bead.created_at), detailColors.text),
    colored("   CREATED ", detailColors.accent, true),
    colored(bead.created_at ?? "unknown", detailColors.muted),
    colored("\nSTATUS ", detailColors.accent, true),
    colored(`${status.mark} ${bead.status}`, status.color),
    colored("   ASSIGNEE ", detailColors.accent, true),
    colored(bead.assignee ?? "unassigned", detailColors.text),
    colored("\nLABELS ", detailColors.accent, true),
    colored(labels, detailColors.text),
  ]
  const queued = queue.findIndex((entry) => entry.id === bead.id)
  if (queued >= 0) {
    chunks.push(
      colored("\nQUEUED ", detailColors.accent, true),
      colored(`#${queued + 1} of ${queue.length} · ${harnessLabel(queue[queued]!.kind, queue[queued]!.model)} · starts when no agent is working`, detailColors.epic),
    )
  }
  if (epic) {
    chunks.push(
      colored("\nEPIC ", detailColors.accent, true),
      colored(`${epic.id}  ${epic.title}`, detailColors.epic, true),
    )
  }
  chunks.push(colored(`\n\n${bead.description || "No description."}`, detailColors.text))
  if (bead.acceptance_criteria) {
    chunks.push(
      colored("\n\nACCEPTANCE CRITERIA\n", detailColors.accent, true),
      colored(bead.acceptance_criteria, detailColors.text),
    )
  }
  chunks.push(
    colored("\n\nBLOCKS\n", detailColors.accent, true),
    ...blocksTreeChunks(bead.id),
  )
  detail.content = new StyledText(chunks)
}

function agentAlive(beadId: string): boolean {
  return launchedAgents.has(beadId) || workflowLaunching.has(beadId)
}

function renderWorkflows(): void {
  workflowBox.visible = workflows.length > 0
  workflowBox.height = workflows.length > 0 ? Math.min(workflows.length, 4) * 2 + 4 : 0
  workflowBox.title = `Epic workflows · ${workflows.length}`
  if (workflows.length === 0) {
    workflowText.content = ""
    return
  }

  const chunks: TextChunk[] = []
  workflows.forEach((workflow, index) => {
    if (index > 0) chunks.push(colored("\n", detailColors.text))
    const epic = issuesByID.get(workflow.epicId)
    const children = childrenByEpic.get(workflow.epicId) ?? []
    const closedCount = children.filter((child) => child.status === "closed").length
    const inReview = (workflow.reviews ?? []).filter((launch) => issuesByID.get(launch.id)?.status !== "closed")
    const reviewingIds = new Set(inReview.map((launch) => launch.id))
    const running = workflow.launched.filter((launch) =>
      issuesByID.get(launch.id)?.status !== "closed" && !reviewingIds.has(launch.id) && agentAlive(launch.id))
    chunks.push(
      colored(workflow.paused ? "⏸ " : "▶ ", workflow.paused ? detailColors.danger : detailColors.closed, true),
      colored(workflow.epicId, detailColors.epic, true),
      colored(`  ${epic?.title ?? ""}`, detailColors.text),
      colored("\n   ", detailColors.muted),
    )
    if (workflow.paused) {
      chunks.push(colored(`paused: ${workflow.paused} · w to resume`, detailColors.danger))
    } else {
      const model = workflow.model ? ` · ${workflow.model.replace(/^claude-/, "")}` : ""
      const active = running.length > 0 ? ` (${running.map((launch) => launch.id).join(", ")})` : ""
      const reviewer = workflow.review
        ? ` · review: ${workflow.review.kind}${workflow.review.model ? ` ${workflow.review.model.replace(/^claude-/, "")}` : ""}`
        : ""
      const reviewing = inReview.length > 0 ? ` · ${inReview.length} in review` : ""
      const branch = workflow.worktree ? ` · ⎇ ${workflow.worktree.branch}` : ""
      chunks.push(colored(
        `${workflow.kind}${model} ×${workflow.concurrency}${branch}${reviewer} · ${closedCount}/${children.length} closed · ${running.length} running${active}${reviewing}`,
        detailColors.muted,
      ))
    }
  })
  workflowText.content = new StyledText(chunks)
}

async function launchWorkflowBead(workflow: Workflow, bead: Bead): Promise<void> {
  if (workflowLaunching.has(bead.id) || launchedAgents.has(bead.id)) return
  workflowLaunching.add(bead.id)
  workflow.launched.push({ id: bead.id, at: Date.now() })
  void saveWorkflows(cwd, workflows)
  optimisticClaims.set(bead.id, { bead })
  agentRevision++
  renderLists()
  showTransientStatus(` Workflow ${workflow.epicId}: starting ${workflow.kind} for ${bead.id}…`, 5_000)
  try {
    const name = await spawnAgent(bead, workflow.kind, cwd, {
      focus: false, model: workflow.model, review: Boolean(workflow.review),
      workdir: workflow.worktree?.path,
    })
    launchedAgents.set(bead.id, name)
    agentRevision++
    renderLists()
  } catch (error) {
    workflow.launched = workflow.launched.filter((launch) => launch.id !== bead.id)
    workflow.paused = `could not start agent for ${bead.id}: ${error instanceof Error ? error.message : String(error)}`
    optimisticClaims.delete(bead.id)
    agentRevision++
    void saveWorkflows(cwd, workflows)
    renderLists()
  } finally {
    workflowLaunching.delete(bead.id)
    renderWorkflows()
  }
}

async function launchWorkflowReview(workflow: Workflow, bead: Bead): Promise<void> {
  const binding = reviewBindingId(bead.id)
  const review = workflow.review
  if (!review || workflowLaunching.has(binding) || launchedAgents.has(binding)) return
  workflowLaunching.add(binding)
  workflow.reviews = [...(workflow.reviews ?? []), { id: bead.id, at: Date.now() }]
  void saveWorkflows(cwd, workflows)
  showTransientStatus(` Workflow ${workflow.epicId}: starting ${review.kind} review of ${bead.id}…`, 5_000)
  try {
    const name = await spawnReviewer(bead, review.kind, cwd, {
      focus: false, model: review.model, workdir: workflow.worktree?.path,
    })
    launchedAgents.set(binding, name)
    agentRevision++
    renderLists()
  } catch (error) {
    workflow.reviews = (workflow.reviews ?? []).filter((launch) => launch.id !== bead.id)
    workflow.paused = `could not start reviewer for ${bead.id}: ${error instanceof Error ? error.message : String(error)}`
    agentRevision++
    void saveWorkflows(cwd, workflows)
  } finally {
    workflowLaunching.delete(binding)
    renderWorkflows()
  }
}

// Beads an agent is actively working: a live Herdr agent bound to a bead that
// is still in progress, plus launches in flight. Optimistic claims count too —
// a bead launched seconds ago has not reached `bd list --status in_progress`
// yet. The designer is not attached to a bead, so it never blocks the queue.
function blockingBeads(): Set<string> {
  return blockingBeadIds({
    bindings: launchedAgents.keys(),
    inProgressIds: [...inProgress.map((bead) => bead.id), ...optimisticClaims.keys()],
    launching: [...workflowLaunching, ...queueLaunching],
    exempt: worktreeExemptIds(workflows),
  })
}

function queuePosition(beadId: string): number {
  return queue.findIndex((entry) => entry.id === beadId)
}

function toggleQueue(kind: AgentKind, model?: string): void {
  const bead = activeList === "ready" ? selectedReady() : undefined
  if (!bead) {
    showTransientStatus(" Switch to the ready list to queue a bead")
    return
  }
  const index = queuePosition(bead.id)
  if (index >= 0) {
    queue = queue.filter((entry) => entry.id !== bead.id)
    showTransientStatus(` Unqueued ${bead.id} · ${queue.length} queued`)
  } else {
    queue = [...queue, { cwd, id: bead.id, kind, model, at: Date.now() }]
    showTransientStatus(` Queued ${bead.id} (#${queue.length}) · runs when no agent is working`)
  }
  void saveQueue(cwd, queue)
  renderLists()
}

async function launchQueued(entry: QueueEntry, bead: Bead): Promise<void> {
  queueLaunching.add(bead.id)
  queue = queue.filter((item) => item.id !== bead.id)
  void saveQueue(cwd, queue)
  optimisticClaims.set(bead.id, { bead })
  agentRevision++
  renderLists()
  showTransientStatus(` Queue: starting ${harnessLabel(entry.kind, entry.model)} for ${bead.id}…`, 5_000)
  try {
    const name = await spawnAgent(bead, entry.kind, cwd, { focus: false, model: entry.model })
    launchedAgents.set(bead.id, name)
    agentRevision++
    showTransientStatus(` Queue: started ${bead.id} · ${queue.length} still queued`, 5_000)
  } catch (error) {
    // Put it back at the front so the queue retries rather than silently
    // dropping work the user asked for.
    queue = [entry, ...queue]
    void saveQueue(cwd, queue)
    optimisticClaims.delete(bead.id)
    agentRevision++
    showTransientStatus(` Queue paused: ${error instanceof Error ? error.message : String(error)}`, 8_000)
  } finally {
    queueLaunching.delete(bead.id)
    renderLists()
  }
}

function runQueue(): void {
  if (queue.length === 0) return
  const pruned = pruneQueue(queue, issuesByID)
  if (pruned.length !== queue.length) {
    queue = pruned
    void saveQueue(cwd, queue)
  }
  if (launching) return
  const readyById = new Map(workflowReady.map((bead) => [bead.id, bead]))
  const entry = nextQueued(queue, { readyIds: new Set(readyById.keys()), activeAgents: blockingBeads().size })
  const bead = entry ? readyById.get(entry.id) : undefined
  if (entry && bead) void launchQueued(entry, bead)
}

function runWorkflows(): void {
  if (workflows.length > 0 && issuesByID.size > 0) {
    const next: Workflow[] = []
    let changed = false
    for (const workflow of workflows) {
      const children = childrenByEpic.get(workflow.epicId) ?? []
      const readyIds = new Set(workflowReady
        .filter((bead) => epicByChild.get(bead.id)?.id === workflow.epicId)
        .map((bead) => bead.id))
      const { workflow: advanced, launches, reviews } = advanceWorkflow(workflow, children, readyIds, agentAlive)
      if (!advanced) {
        changed = true
        const merge = workflow.worktree ? ` · merge ⎇ ${workflow.worktree.branch} when ready` : ""
        showTransientStatus(` Workflow done: all beads in ${workflow.epicId} are closed${merge}`, 8_000)
        continue
      }
      if (advanced !== workflow) changed = true
      next.push(advanced)
      for (const bead of launches) void launchWorkflowBead(advanced, bead)
      for (const bead of reviews) void launchWorkflowReview(advanced, bead)
    }
    workflows = next
    if (changed) void saveWorkflows(cwd, workflows)
  }
  renderWorkflows()
}

type ModalValue =
  | { type: "kind", kind: AgentKind, model?: string }
  | { type: "concurrency", value: number }
  | { type: "worktree", worktree?: WorktreeConfig }
  | { type: "review", review?: ReviewConfig }
  | { type: "edit-review" }
  | { type: "edit-concurrency" }
  | { type: "resume" }
  | { type: "cancel" }

function setModalOptions(options: { name: string, description: string, value: ModalValue }[]): void {
  modalSelect.options = options
  modalBox.height = options.length * 2 + 6
  modalSelect.setSelectedIndex(0)
}

function closeModal(): void {
  modalState = null
  modalBox.visible = false
  focusList(activeList)
}

function harnessLabel(kind: AgentKind, model?: string): string {
  if (kind !== "claude") return kind
  return model ? `claude · ${model.replace(/^claude-/, "")}` : "claude"
}

function showHarnessStep(): void {
  setModalOptions([
    { name: "Claude · Fable", description: "claude --model claude-fable-5", value: { type: "kind", kind: "claude", model: "claude-fable-5" } },
    { name: "Claude · Opus 5", description: "claude --model claude-opus-5", value: { type: "kind", kind: "claude", model: "claude-opus-5" } },
    { name: "Pi", description: "pi", value: { type: "kind", kind: "pi" } },
  ])
}

function showConcurrencyStep(): void {
  setModalOptions([1, 2, 3].map((count) => ({
    name: count === 1 ? "Sequential · one bead at a time" : `${count} beads at a time`,
    description: count === 1
      ? "waits for each bead to close before launching the next"
      : "agents share the working tree — beware conflicting edits",
    value: { type: "concurrency", value: count } as ModalValue,
  })))
}

function showWorktreeStep(epic: Bead): void {
  const defaults = worktreeDefaults(cwd, epic.id)
  setModalOptions([
    {
      name: "Current branch",
      description: "agents work in your working tree and block other launches",
      value: { type: "worktree" },
    },
    {
      name: `Worktree · ${defaults.branch}`,
      description: `isolated at ${defaults.path} · beads outside this workflow can run in parallel`,
      value: { type: "worktree", worktree: defaults },
    },
  ])
}

function showReviewStep(): void {
  const check = "reviewer verifies the commit against acceptance criteria, fixes problems, and closes the bead"
  setModalOptions([
    { name: "No review", description: "workers close their own beads", value: { type: "review" } },
    { name: "Check with Claude · Fable", description: check, value: { type: "review", review: { kind: "claude", model: "claude-fable-5" } } },
    { name: "Check with Claude · Opus 5", description: check, value: { type: "review", review: { kind: "claude", model: "claude-opus-5" } } },
    { name: "Check with Pi", description: check, value: { type: "review", review: { kind: "pi" } } },
  ])
}

function showEditMenu(workflow: Workflow): void {
  setModalOptions([
    ...(workflow.paused ? [{
      name: "Resume workflow",
      description: `paused: ${workflow.paused}`,
      value: { type: "resume" } as ModalValue,
    }] : []),
    {
      name: "Change review step",
      description: `currently: ${workflow.review ? `check with ${harnessLabel(workflow.review.kind, workflow.review.model)}` : "no review"} · applies to beads launched from now on`,
      value: { type: "edit-review" },
    },
    {
      name: "Change concurrency",
      description: `currently ×${workflow.concurrency}`,
      value: { type: "edit-concurrency" },
    },
    {
      name: "Cancel workflow",
      description: `stop launching new beads · agents already running keep going${workflow.worktree ? ` · worktree ${workflow.worktree.path} is kept` : ""}`,
      value: { type: "cancel" },
    },
  ])
}

function openWorkflowModalFor(epic: Bead): void {
  modalState = { epic }
  modalBox.title = `Epic workflow · ${epic.id}`
  modalHeader.content = epic.title
  const existing = workflows.find((workflow) => workflow.epicId === epic.id)
  if (existing) showEditMenu(existing)
  else showHarnessStep()
  modalBox.visible = true
  modalSelect.focus()
}

function openWorkflowModal(): void {
  const current = selected()
  const epic = current?.issue_type === "epic" ? current : current ? epicByChild.get(current.id) : undefined
  if (!epic) {
    showTransientStatus(" Select an epic (e view) or a bead that belongs to one")
    return
  }
  openWorkflowModalFor(epic)
}

async function startWorkflow(epic: Bead, kind: AgentKind, model: string | undefined, concurrency: number, review?: ReviewConfig, worktree?: WorktreeConfig): Promise<void> {
  if (worktree) {
    updateChrome(` Preparing worktree ${worktree.branch}…`)
    try {
      await ensureWorktree(cwd, worktree)
    } catch (error) {
      showTransientStatus(` Worktree failed, workflow not started: ${error instanceof Error ? error.message : String(error)}`, 8_000)
      return
    }
  }
  const workflow: Workflow = { cwd, epicId: epic.id, kind, concurrency, launched: [] }
  if (model) workflow.model = model
  if (review) workflow.review = review
  if (worktree) workflow.worktree = worktree
  workflows.push(workflow)
  void saveWorkflows(cwd, workflows)
  renderWorkflows()
  showTransientStatus(` Workflow started for ${epic.id} · ${kind} ×${concurrency}${worktree ? ` · ⎇ ${worktree.branch}` : ""}${review ? ` · review: ${review.kind}` : ""}`, 5_000)
  void refresh(true)
}

modalSelect.on(SelectRenderableEvents.ITEM_SELECTED, () => {
  const state = modalState
  const value = modalSelect.getSelectedOption()?.value as ModalValue | undefined
  if (!state || !value) return
  const activeWorkflow = () => workflows.find((workflow) => workflow.epicId === state.epic.id)

  if (value.type === "kind") {
    modalState = { ...state, kind: value.kind, model: value.model }
    showConcurrencyStep()
    return
  }
  if (value.type === "concurrency") {
    if (state.edit) {
      const workflow = activeWorkflow()
      if (workflow) {
        workflow.concurrency = value.value
        void saveWorkflows(cwd, workflows)
        showTransientStatus(` Workflow ${state.epic.id}: concurrency set to ×${value.value}`)
      }
      closeModal()
      renderWorkflows()
      return
    }
    modalState = { ...state, concurrency: value.value }
    showWorktreeStep(state.epic)
    return
  }
  if (value.type === "worktree") {
    modalState = { ...state, worktree: value.worktree }
    showReviewStep()
    return
  }
  if (value.type === "edit-review") {
    modalState = { ...state, edit: true }
    showReviewStep()
    return
  }
  if (value.type === "edit-concurrency") {
    modalState = { ...state, edit: true }
    showConcurrencyStep()
    return
  }

  closeModal()
  if (value.type === "review") {
    if (state.edit) {
      const workflow = activeWorkflow()
      if (workflow) {
        if (value.review) workflow.review = value.review
        else delete workflow.review
        void saveWorkflows(cwd, workflows)
        renderWorkflows()
        showTransientStatus(` Workflow ${state.epic.id}: review ${value.review ? `by ${harnessLabel(value.review.kind, value.review.model)}` : "disabled"} for beads launched from now on`, 5_000)
      }
    } else if (state.kind && state.concurrency) {
      void startWorkflow(state.epic, state.kind, state.model, state.concurrency, value.review, state.worktree)
    }
  } else if (value.type === "resume") {
    const index = workflows.findIndex((workflow) => workflow.epicId === state.epic.id)
    const existing = index >= 0 ? workflows[index] : undefined
    if (existing) {
      workflows[index] = resumeWorkflow(existing, childrenByEpic.get(state.epic.id) ?? [], agentAlive)
      void saveWorkflows(cwd, workflows)
      showTransientStatus(` Workflow resumed for ${state.epic.id}`)
      void refresh(true)
    }
  } else if (value.type === "cancel") {
    workflows = workflows.filter((workflow) => workflow.epicId !== state.epic.id)
    void saveWorkflows(cwd, workflows)
    renderWorkflows()
    showTransientStatus(` Workflow cancelled for ${state.epic.id}`)
  }
})

async function refresh(silent = false): Promise<void> {
  if (refreshing) return
  refreshing = true
  const discoveryRevision = agentRevision
  if (!silent) updateChrome(" Loading beads…")
  updateDetail()
  try {
    const [loadedBeads, loadedInProgress, graphIssues] = await Promise.all([
      loadBeads(cwd, view),
      loadInProgress(cwd),
      loadIssueGraph(cwd),
    ])
    beads = loadedBeads
    inProgress = loadedInProgress
    rebuildIssueGraph(graphIssues)
    // Workflows always react to the ready list, even while the epics view has
    // `beads` holding epics instead of ready work.
    workflowReady = view === "ready"
      ? [...loadedBeads]
      : workflows.length > 0 || queue.length > 0 ? await loadBeads(cwd, "ready") : []
    const discovered = await discoverAgentBindings(cwd, inProgress)
    const settled = !launching && workflowLaunching.size === 0 && queueLaunching.size === 0
    if (discoveryRevision === agentRevision && settled) launchedAgents.clear()
    for (const [beadId, name] of discovered) launchedAgents.set(beadId, name)
    if (discoveryRevision === agentRevision && settled) {
      for (const beadId of optimisticClaims.keys()) {
        if (!launchedAgents.has(beadId)) optimisticClaims.delete(beadId)
      }
    }
    for (const bead of beads) {
      if (launchedAgents.has(bead.id) && !inProgress.some((active) => active.id === bead.id)) {
        optimisticClaims.set(bead.id, { bead })
      }
    }
    runWorkflows()
    runQueue()
    // Capture selection now, not when refresh began. The user may have moved
    // the cursor while the bd/Herdr subprocesses were running.
    renderLists()
    if (silent) updateTitles()
    else updateChrome()
  } catch (error) {
    if (!silent) updateChrome(` Error: ${error instanceof Error ? error.message : String(error)}`)
  } finally {
    refreshing = false
    updateDetail()
  }
}

async function launch(kind: AgentKind): Promise<void> {
  const bead = activeList === "ready" ? selectedReady() : undefined
  if (!bead || launching) {
    if (activeList === "progress") showTransientStatus(" Switch to the ready list to launch an agent")
    return
  }

  if (queuePosition(bead.id) >= 0) {
    queue = queue.filter((entry) => entry.id !== bead.id)
    void saveQueue(cwd, queue)
  }
  // Optimistic update: move the bead immediately while the new agent starts.
  optimisticClaims.set(bead.id, { bead })
  renderLists({ ready: bead.id, progress: selectedProgress()?.id })
  launching = true
  agentRevision++
  updateChrome(` Starting ${kind} for ${bead.id} with Herdr…`)
  try {
    const name = await spawnAgent(bead, kind, cwd)
    launchedAgents.set(bead.id, name)
    agentRevision++
    renderLists()
    showTransientStatus(` Started ${kind} for ${bead.id}`)
  } catch (error) {
    agentRevision++
    optimisticClaims.delete(bead.id)
    inProgress = inProgress.filter((item) => item.id !== bead.id)
    beads = [bead, ...beads]
    renderLists({ ready: bead.id, progress: selectedProgress()?.id })
    updateChrome(` Error: ${error instanceof Error ? error.message : String(error)}`)
  } finally {
    launching = false
  }
}

async function goToDesigner(): Promise<void> {
  const existing = launchedAgents.get(DESIGNER_ID)
  if (existing) {
    try {
      await focusAgent(existing, cwd)
    } catch (error) {
      showTransientStatus(` Error: ${error instanceof Error ? error.message : String(error)}`, 5_000)
    }
    return
  }

  if (launching) return
  launching = true
  agentRevision++
  updateChrome(` Starting bead designer with Herdr…`)
  try {
    const name = await spawnDesigner("claude", cwd)
    launchedAgents.set(DESIGNER_ID, name)
    agentRevision++
    showTransientStatus(" Started bead designer")
  } catch (error) {
    agentRevision++
    updateChrome(` Error: ${error instanceof Error ? error.message : String(error)}`)
  } finally {
    launching = false
  }
}

async function goToAgent(): Promise<void> {
  const bead = activeList === "progress" ? selectedProgress() : undefined
  // Prefer the reviewer once one exists — it is the agent actively working.
  const name = bead ? launchedAgents.get(reviewBindingId(bead.id)) ?? launchedAgents.get(bead.id) : undefined
  if (!bead || !name) {
    showTransientStatus(" Select an in-progress bead launched by this viewer")
    return
  }
  try {
    await focusAgent(name, cwd)
  } catch (error) {
    showTransientStatus(` Error: ${error instanceof Error ? error.message : String(error)}`, 5_000)
  }
}

list.on(SelectRenderableEvents.SELECTION_CHANGED, () => updateDetail())
progressList.on(SelectRenderableEvents.SELECTION_CHANGED, () => updateDetail())

// SelectRenderable 0.5 handles keyboard selection but does not implement mouse
// interaction. Its rows are two terminal lines tall (name + description), and
// it keeps the selected row centered whenever possible.
function visibleScrollOffset(select: SelectRenderable): number {
  const visibleItems = Math.max(1, Math.floor(select.height / 2))
  return Math.max(0, Math.min(
    select.getSelectedIndex() - Math.floor(visibleItems / 2),
    select.options.length - visibleItems,
  ))
}

function addMouseSelection(select: SelectRenderable, target: "ready" | "progress"): void {
  select.onMouseDown = (event) => {
    if (modalState || event.button !== 0) return
    event.preventDefault()
    if (activeList !== target) focusList(target)

    const relativeY = event.y - select.screenY
    const visibleItems = Math.max(1, Math.floor(select.height / 2))
    const visibleRow = Math.floor(relativeY / 2)
    if (relativeY < 0 || visibleRow < 0 || visibleRow >= visibleItems) return

    const index = visibleScrollOffset(select) + visibleRow
    if (index < select.options.length) select.setSelectedIndex(index)
  }

  select.onMouseScroll = (event) => {
    const direction = event.scroll?.direction
    if (modalState || (direction !== "up" && direction !== "down")) return
    event.preventDefault()
    if (activeList !== target) focusList(target)
    if (select.options.length === 0) return

    const delta = Math.max(1, Math.round(event.scroll?.delta ?? 1))
    const change = direction === "up" ? -delta : delta
    const index = Math.max(0, Math.min(select.options.length - 1, select.getSelectedIndex() + change))
    if (index !== select.getSelectedIndex()) select.setSelectedIndex(index)
  }
}

addMouseSelection(list, "ready")
addMouseSelection(progressList, "progress")

// Each workflow renders as two text lines; map a click back to its entry.
workflowText.onMouseDown = (event) => {
  if (modalState || event.button !== 0) return
  event.preventDefault()
  const index = Math.floor(Math.max(0, event.y - workflowText.screenY - 1) / 2)
  const workflow = workflows[index]
  const epic = workflow ? issuesByID.get(workflow.epicId) : undefined
  if (epic) openWorkflowModalFor(epic)
}
renderer.keyInput.on("keypress", (key: KeyEvent) => {
  if (key.ctrl && key.name === "c") {
    key.preventDefault()
    renderer.destroy()
    return
  }
  if (modalState) {
    // The focused modal select handles ↑/↓/enter itself; everything else is
    // swallowed so list shortcuts cannot fire underneath the modal.
    if (key.name === "escape" || key.name === "q") {
      key.preventDefault()
      closeModal()
    }
    return
  }
  // Vim-style search: `/` opens the prompt, typed characters live-filter both
  // lists, Enter keeps the filter (so c/p/Q can start a match), Esc clears it.
  if (searchInput) {
    if (key.name === "escape") {
      key.preventDefault()
      searchInput = false
      searchQuery = ""
      renderLists()
      updateChrome()
    } else if (key.name === "return" || key.name === "enter") {
      key.preventDefault()
      searchInput = false
      updateChrome()
    } else if (key.name === "backspace") {
      key.preventDefault()
      searchQuery = searchQuery.slice(0, -1)
      renderLists()
      updateChrome()
    } else if (key.name === "up" || key.name === "down" || key.name === "tab") {
      // Fall through so the focused list keeps handling navigation.
      if (key.name === "tab") {
        key.preventDefault()
        focusList(activeList === "ready" ? "progress" : "ready")
      }
    } else if (key.sequence && key.sequence.length === 1 && !key.ctrl && !key.meta && key.sequence >= " ") {
      key.preventDefault()
      searchQuery += key.sequence
      renderLists()
      updateChrome()
    }
    return
  }
  if (key.sequence === "/") {
    key.preventDefault()
    searchInput = true
    searchQuery = ""
    renderLists()
    updateChrome()
    return
  }
  if (key.name === "escape" && searchQuery) {
    key.preventDefault()
    searchQuery = ""
    renderLists()
    updateChrome()
    return
  }
  // Uppercase letters arrive either as a shifted `q` or as a bare `Q`,
  // depending on the terminal's keyboard protocol. Queue is the shifted key
  // and quit the bare one, so a stray `q` never queues work by accident.
  if (key.sequence === "Q" || (key.name === "q" && key.shift)) {
    key.preventDefault()
    toggleQueue("claude")
    return
  }
  if (key.name === "q") {
    key.preventDefault()
    renderer.destroy()
    return
  }
  if (key.name === "tab") {
    key.preventDefault()
    focusList(activeList === "ready" ? "progress" : "ready")
  } else if (key.name === "g") void goToAgent()
  else if (key.name === "r") void refresh()
  else if (key.name === "e") {
    view = view === "ready" ? "epics" : "ready"
    void refresh()
  } else if (key.name === "d") void goToDesigner()
  else if (key.name === "w") openWorkflowModal()
  else if (key.name === "p") void launch("pi")
  else if (key.name === "c" && !key.ctrl) void launch("claude")
})

focusList("ready")
workflows = await loadWorkflows(cwd)
queue = await loadQueue(cwd)
renderWorkflows()
await refresh()
const pollTimer = setInterval(() => void refresh(true), 2_000)
pollTimer.unref()
renderer.start()
