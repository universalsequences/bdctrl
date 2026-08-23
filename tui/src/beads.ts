export type BeadDependency = {
  issue_id: string
  depends_on_id: string
  type: string
}

export type Bead = {
  id: string
  title: string
  description?: string
  design?: string
  acceptance_criteria?: string
  status: string
  priority: number
  issue_type: string
  assignee?: string
  labels?: string[]
  dependencies?: BeadDependency[]
  created_at?: string
  updated_at?: string
}

export type View = "ready" | "epics"

export type CommandResult = { stdout: string, stderr: string, exitCode: number }

export async function runCommandResult(command: string[], cwd: string): Promise<CommandResult> {
  const proc = Bun.spawn(command, { cwd, stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { stdout, stderr, exitCode }
}

export async function runCommand(command: string[], cwd: string): Promise<string> {
  const { stdout, stderr, exitCode } = await runCommandResult(command, cwd)
  if (exitCode !== 0) throw new Error(stderr.trim() || `${command[0]} exited ${exitCode}`)
  return stdout
}

export function parseBeads(raw: string): Bead[] {
  const parsed: unknown = JSON.parse(raw)
  const rows = Array.isArray(parsed)
    ? parsed
    : typeof parsed === "object" && parsed && Array.isArray((parsed as { issues?: unknown }).issues)
      ? (parsed as { issues: unknown[] }).issues
      : []

  return rows
    .filter((row): row is Record<string, unknown> => typeof row === "object" && row !== null)
    .map((row) => ({
      ...row,
      id: String(row.id ?? ""),
      title: String(row.title ?? "Untitled"),
      status: String(row.status ?? "open"),
      priority: Number(row.priority ?? 2),
      issue_type: String(row.issue_type ?? row.type ?? "task"),
    }) as Bead)
    .filter((bead) => bead.id.length > 0)
    .sort((a, b) => (b.created_at ?? "").localeCompare(a.created_at ?? ""))
}

export function parseExport(raw: string): Bead[] {
  const trimmed = raw.trim()
  if (!trimmed) return []
  try {
    return parseBeads(trimmed)
  } catch {
    // `bd export` is newline-delimited JSON rather than a JSON array.
    const rows = trimmed.split("\n").filter(Boolean).map((line) => JSON.parse(line))
    return parseBeads(JSON.stringify(rows))
  }
}

export async function loadIssueGraph(cwd: string): Promise<Bead[]> {
  const result = await runCommandResult(["bd", "export"], cwd)
  // Keep basic browsing functional with older bd versions that do not expose
  // export; epic and blocking metadata will simply be absent.
  return result.exitCode === 0 ? parseExport(result.stdout) : []
}

export async function loadBeads(cwd: string, view: View): Promise<Bead[]> {
  const args = view === "ready"
    ? ["bd", "ready", "--json", "--sort", "oldest", "--exclude-type", "epic", "--limit", "0"]
    : ["bd", "list", "--json", "--type", "epic", "--status", "open", "--sort", "created", "--limit", "0"]
  return parseBeads(await runCommand(args, cwd))
}

export async function loadInProgress(cwd: string): Promise<Bead[]> {
  const args = ["bd", "list", "--json", "--status", "in_progress", "--sort", "updated", "--reverse", "--limit", "0"]
  const rows = parseBeads(await runCommand(args, cwd))
  return rows.sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? ""))
}

// Case-insensitive multi-term match: every whitespace-separated term must
// appear somewhere in the bead (or its epic's id/title).
export function matchesBead(bead: Bead, query: string, epic?: Bead): boolean {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (terms.length === 0) return true
  const haystack = [
    bead.id, bead.title, bead.description, bead.acceptance_criteria,
    bead.assignee, ...(bead.labels ?? []), epic?.id, epic?.title,
  ].filter(Boolean).join(" ").toLowerCase()
  return terms.every((term) => haystack.includes(term))
}

export function age(createdAt?: string, now = Date.now()): string {
  if (!createdAt) return "unknown age"
  const elapsed = now - Date.parse(createdAt)
  if (!Number.isFinite(elapsed) || elapsed < 0) return "unknown age"
  const minutes = Math.floor(elapsed / 60_000)
  if (minutes < 60) return `${minutes}m old`
  const hours = Math.floor(minutes / 60)
  if (hours < 48) return `${hours}h old`
  return `${Math.floor(hours / 24)}d old`
}
