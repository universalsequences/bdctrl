import type { Bead } from "./beads"
import { runCommand, runCommandResult } from "./beads"
import { mkdir } from "node:fs/promises"
import { homedir } from "node:os"
import { dirname, join, resolve } from "node:path"

export type AgentKind = "pi" | "claude"

// Sentinel binding ID for the bead-designer agent, which is not tied to a bead.
export const DESIGNER_ID = "__designer__"
const DESIGNER_BASE = "designer"

type HerdrResponse = {
  result?: {
    pane?: { pane_id?: string }
    root_pane?: { pane_id?: string }
  }
}

type AgentBinding = { cwd: string, beadId: string, name: string }
type LiveAgent = { name?: string, cwd?: string, interactive_ready?: boolean }
type AgentListResponse = { result?: { agents?: LiveAgent[] } }

export const stateRoot = join(process.env.XDG_STATE_HOME ?? join(homedir(), ".local", "state"), "beadsviewer")

// One state file per project, so two viewers running against different repos
// never read-modify-write the same file concurrently.
export function stateFilePath(cwd: string, name: string): string {
  return join(stateRoot, `${Bun.hash(resolve(cwd)).toString(36)}-${name}`)
}

// Returns null when the file is missing or unreadable, [] only when it
// genuinely holds an empty list — callers use null to fall back to the legacy
// shared file from before per-project state.
export async function readStateArray<T>(path: string): Promise<T[] | null> {
  try {
    const value: unknown = await Bun.file(path).json()
    return Array.isArray(value) ? value as T[] : null
  } catch {
    return null
  }
}

function safeAgentBase(beadId: string): string {
  const id = beadId.toLowerCase().replace(/[^a-z0-9_-]/g, "-")
  return /^[a-z]/.test(id) ? id : `bead-${id}`
}

export function agentName(beadId: string, now = Date.now()): string {
  const base = safeAgentBase(beadId)
  const suffix = `-${now.toString(36).slice(-5)}`
  return `${base.slice(0, 32 - suffix.length)}${suffix}`
}

async function readBindings(cwd: string): Promise<AgentBinding[]> {
  const scoped = await readStateArray<AgentBinding>(stateFilePath(cwd, "agents.json"))
  if (scoped) return scoped
  const project = resolve(cwd)
  const legacy = await readStateArray<AgentBinding>(join(stateRoot, "agents.json"))
  return (legacy ?? []).filter((item) => item.cwd === project)
}

async function rememberAgent(cwd: string, beadId: string, name: string): Promise<void> {
  const bindings = (await readBindings(cwd)).filter((item) => item.beadId !== beadId)
  bindings.push({ cwd: resolve(cwd), beadId, name })
  const path = stateFilePath(cwd, "agents.json")
  await mkdir(dirname(path), { recursive: true })
  await Bun.write(path, JSON.stringify(bindings, null, 2) + "\n")
}

export async function discoverAgentBindings(cwd: string, beads: Bead[]): Promise<Map<string, string>> {
  const result = new Map<string, string>()
  if (process.env.HERDR_ENV !== "1") return result

  try {
    const response = JSON.parse(await runCommand(["herdr", "agent", "list"], cwd)) as AgentListResponse
    const live = (response.result?.agents ?? []).filter((agent): agent is LiveAgent & { name: string } => Boolean(agent.name))
    const liveNames = new Set(live.map((agent) => agent.name))
    const project = resolve(cwd)

    for (const binding of await readBindings(cwd)) {
      if (binding.cwd === project && liveNames.has(binding.name)) result.set(binding.beadId, binding.name)
    }

    // Agent names have always included the bead ID, so this also recovers
    // agents launched before the state file existed.
    for (const bead of beads) {
      if (result.has(bead.id)) continue
      const prefix = `${safeAgentBase(bead.id).slice(0, 26)}-`
      const match = live.find((agent) => agent.cwd && resolve(agent.cwd) === project && agent.name.startsWith(prefix))
      if (match) {
        result.set(bead.id, match.name)
        await rememberAgent(cwd, bead.id, match.name)
      }
    }

    if (!result.has(DESIGNER_ID)) {
      const prefix = `${DESIGNER_BASE}-`
      const match = live.find((agent) => agent.cwd && resolve(agent.cwd) === project && agent.name.startsWith(prefix))
      if (match) {
        result.set(DESIGNER_ID, match.name)
        await rememberAgent(cwd, DESIGNER_ID, match.name)
      }
    }
  } catch {
    // Herdr discovery is optional; the Beads viewer still works without it.
  }
  return result
}

export function taskPrompt(bead: Bead, review = false): string {
  const finish = review
    ? `Do NOT close the bead: instead run \`bd update ${bead.id} --add-label needs-review\` and leave it in progress — a separate reviewer agent will verify your work and close it.`
    : `Update/close the bead when appropriate.`
  return `Take on bead ${bead.id}: ${bead.title}. Start by running \`bd show ${bead.id}\`, then atomically claim it with \`bd update ${bead.id} --claim\`. Implement the bead in this repository and run the relevant checks. When the work is complete, review the diff and create a git commit containing only the changes for this bead with a clear commit message. ${finish}`
}

export function reviewPrompt(bead: Bead): string {
  return `You are the reviewer for bead ${bead.id}: ${bead.title}. A worker agent has implemented it and committed the changes. Start with \`bd show ${bead.id}\` to read the description and acceptance criteria, then find the commit(s) for this bead in recent git history and review them against those criteria. Run the relevant checks yourself. If you find problems, fix them directly and commit the fixes with a clear message referencing ${bead.id}. When you are satisfied the bead is genuinely done, run \`bd update ${bead.id} --remove-label needs-review\` and close it with \`bd close ${bead.id}\`. Do not close it until the checks pass.`
}

export function designerPrompt(): string {
  return [
    "You are the bead designer for this repository. Your job is to turn ideas and discussion into well-scoped beads using the `bd` CLI — you never implement anything yourself.",
    "Start by running `bd ready --json --limit 0` and `bd list --json --status in_progress --limit 0` to see the current state, then wait for direction.",
    "When given an idea, work it out into one or more beads with `bd create`, each with a clear description and acceptance criteria a separate worker agent could implement without further context. Split large work into an epic with dependent beads (`bd dep`), and set sensible priorities.",
    "Do not edit source files, do not claim beads, and do not close beads you did not author — implementation is done by worker agents that consume ready beads.",
  ].join(" ")
}

export function herdrError(stdout: string): { code?: string, message?: string } | undefined {
  try {
    const parsed = JSON.parse(stdout) as { error?: { code?: string, message?: string } }
    return parsed.error
  } catch {
    return undefined
  }
}

// A freshly created pane's shell is usually not at its interactive prompt yet,
// and herdr rejects `agent start` with agent_pane_busy (exit 1, error JSON on
// stderr) instead of waiting. Retry until the shell is ready.
async function startAgentInPane(name: string, kind: AgentKind, pane: string, cwd: string, agentArgs: string[] = []): Promise<void> {
  const deadline = Date.now() + 30_000
  while (true) {
    const { stdout, stderr, exitCode } = await runCommandResult(
      ["herdr", "agent", "start", name, "--kind", kind, "--pane", pane, ...(agentArgs.length > 0 ? ["--", ...agentArgs] : [])], cwd,
    )
    if (exitCode === 0) return
    const error = herdrError(stderr) ?? herdrError(stdout)
    if (error?.code !== "agent_pane_busy" || Date.now() >= deadline) {
      throw new Error(error?.message ?? (stderr.trim() || `herdr agent start exited ${exitCode}`))
    }
    await Bun.sleep(300)
  }
}

// A freshly started agent process is not reading stdin yet, and a prompt sent
// before it is drops silently (observed with pi: the tab opens but the agent
// never receives its task). Wait until herdr reports the agent interactive,
// then fall through on timeout — a late prompt is better than none.
async function waitForInteractive(name: string, cwd: string): Promise<void> {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    try {
      const response = JSON.parse(await runCommand(["herdr", "agent", "list"], cwd)) as AgentListResponse
      const agent = (response.result?.agents ?? []).find((candidate) => candidate.name === name)
      if (agent?.interactive_ready) return
    } catch {
      // Transient list failures should not abort the launch; keep waiting.
    }
    await Bun.sleep(300)
  }
}

function paneId(raw: string): string {
  const response = JSON.parse(raw) as HerdrResponse
  const id = response.result?.pane?.pane_id ?? response.result?.root_pane?.pane_id
  if (!id) throw new Error("Herdr did not return a pane ID")
  return id
}

async function startHerdrAgent(options: {
  bindingId: string, label: string, prompt: string, kind: AgentKind, cwd: string,
  agentArgs?: string[], focus?: boolean, workdir?: string,
}): Promise<string> {
  const { bindingId, label, prompt, kind, cwd, agentArgs = [], focus = true, workdir } = options
  if (process.env.HERDR_ENV !== "1") {
    throw new Error("Run beadsviewer inside a Herdr pane to launch an agent")
  }

  // Agents always get their own tab — never a pane split. The tab runs in
  // `workdir` (a workflow's worktree) when given, but bindings are still
  // recorded under the project `cwd` so discovery and stall detection see them.
  const create = [
    "herdr", "tab", "create",
    ...(process.env.HERDR_WORKSPACE_ID ? ["--workspace", process.env.HERDR_WORKSPACE_ID] : []),
    "--cwd", workdir ?? cwd, "--label", label, "--no-focus",
  ]

  const id = paneId(await runCommand(create, cwd))
  const name = agentName(label)
  await startAgentInPane(name, kind, id, cwd, agentArgs)
  await rememberAgent(cwd, bindingId, name)
  await waitForInteractive(name, cwd)
  await runCommand(["herdr", "agent", "prompt", name, prompt], cwd)
  if (focus) await runCommand(["herdr", "agent", "focus", name], cwd)
  return name
}

export type SpawnOptions = { focus?: boolean, model?: string, review?: boolean, workdir?: string }

export async function spawnAgent(bead: Bead, kind: AgentKind, cwd: string, options: SpawnOptions = {}): Promise<string> {
  const agentArgs = kind === "claude" && options.model ? ["--model", options.model] : []
  return startHerdrAgent({
    bindingId: bead.id, label: bead.id, prompt: taskPrompt(bead, options.review), kind, cwd,
    agentArgs, focus: options.focus, workdir: options.workdir,
  })
}

export async function spawnReviewer(bead: Bead, kind: AgentKind, cwd: string, options: SpawnOptions = {}): Promise<string> {
  const agentArgs = kind === "claude" && options.model ? ["--model", options.model] : []
  return startHerdrAgent({
    bindingId: `review:${bead.id}`, label: `rev-${bead.id}`, prompt: reviewPrompt(bead), kind, cwd,
    agentArgs, focus: options.focus, workdir: options.workdir,
  })
}

export async function spawnDesigner(kind: AgentKind, cwd: string): Promise<string> {
  return startHerdrAgent({ bindingId: DESIGNER_ID, label: DESIGNER_BASE, prompt: designerPrompt(), kind, cwd })
}

export async function focusAgent(name: string, cwd: string): Promise<void> {
  if (process.env.HERDR_ENV !== "1") throw new Error("Run beadsviewer inside a Herdr pane")
  await runCommand(["herdr", "agent", "focus", name], cwd)
}
