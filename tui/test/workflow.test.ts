import { describe, expect, test } from "bun:test"
import type { Bead } from "../src/beads"
import { advanceWorkflow, resumeWorkflow, reviewBindingId, REVIEW_LABEL, STALL_GRACE_MS, worktreeDefaults, worktreeExemptIds, type Workflow } from "../src/workflow"

const NOW = Date.parse("2026-08-18T00:00:00Z")

function child(id: string, status = "open", priority = 2, createdAt = "2026-08-01T00:00:00Z"): Bead {
  return { id, title: id, status, priority, issue_type: "task", created_at: createdAt }
}

function workflow(overrides: Partial<Workflow> = {}): Workflow {
  return { cwd: "/repo", epicId: "epic-1", kind: "claude", concurrency: 1, launched: [], ...overrides }
}

const dead = () => false
const alive = () => true

describe("advanceWorkflow", () => {
  test("launches ready children up to concurrency, best priority first", () => {
    const children = [child("a", "open", 2), child("b", "open", 1), child("c", "open", 1, "2026-07-01T00:00:00Z")]
    const ready = new Set(["a", "b", "c"])

    const sequential = advanceWorkflow(workflow(), children, ready, dead, NOW)
    expect(sequential.launches.map((bead) => bead.id)).toEqual(["c"])

    const concurrent = advanceWorkflow(workflow({ concurrency: 2 }), children, ready, dead, NOW)
    expect(concurrent.launches.map((bead) => bead.id)).toEqual(["c", "b"])
  })

  test("does not relaunch beads it already launched", () => {
    const wf = workflow({ concurrency: 2, launched: [{ id: "a", at: NOW }] })
    const result = advanceWorkflow(wf, [child("a"), child("b")], new Set(["a", "b"]), alive, NOW)
    expect(result.launches.map((bead) => bead.id)).toEqual(["b"])
  })

  test("waits while a running launch fills the only slot", () => {
    const wf = workflow({ launched: [{ id: "a", at: NOW - 10_000 }] })
    const result = advanceWorkflow(wf, [child("a", "in_progress"), child("b")], new Set(["b"]), alive, NOW)
    expect(result.launches).toEqual([])
    expect(result.workflow?.paused).toBeUndefined()
  })

  test("launches the next bead once the previous one closes", () => {
    const wf = workflow({ launched: [{ id: "a", at: NOW - 60_000 }] })
    const result = advanceWorkflow(wf, [child("a", "closed"), child("b")], new Set(["b"]), dead, NOW)
    expect(result.launches.map((bead) => bead.id)).toEqual(["b"])
  })

  test("pauses when a launched agent dies without closing its bead", () => {
    const wf = workflow({ launched: [{ id: "a", at: NOW - STALL_GRACE_MS - 1 }] })
    const result = advanceWorkflow(wf, [child("a", "in_progress"), child("b")], new Set(["b"]), dead, NOW)
    expect(result.launches).toEqual([])
    expect(result.workflow?.paused).toContain("a")
  })

  test("grace period protects a launch whose agent has not appeared yet", () => {
    const wf = workflow({ launched: [{ id: "a", at: NOW - 5_000 }] })
    const result = advanceWorkflow(wf, [child("a"), child("b")], new Set(["b"]), dead, NOW)
    expect(result.workflow?.paused).toBeUndefined()
    expect(result.launches).toEqual([])
  })

  test("completes when every child is closed", () => {
    const result = advanceWorkflow(workflow(), [child("a", "closed"), child("b", "closed")], new Set(), dead, NOW)
    expect(result.workflow).toBeNull()
  })

  test("pauses when open beads remain but nothing is ready or moving", () => {
    const result = advanceWorkflow(workflow(), [child("a", "closed"), child("b", "blocked")], new Set(), dead, NOW)
    expect(result.workflow?.paused).toContain("none ready")
  })

  test("waits instead of pausing while a manually claimed child is in progress", () => {
    const result = advanceWorkflow(workflow(), [child("a", "in_progress"), child("b")], new Set(), dead, NOW)
    expect(result.workflow?.paused).toBeUndefined()
    expect(result.launches).toEqual([])
  })

  test("a paused workflow launches nothing until resumed", () => {
    const wf = workflow({ paused: "boom" })
    const result = advanceWorkflow(wf, [child("a")], new Set(["a"]), dead, NOW)
    expect(result.launches).toEqual([])
    expect(result.workflow).toBe(wf)
  })

  test("waits without judging when the epic has no known children yet", () => {
    const result = advanceWorkflow(workflow(), [], new Set(), dead, NOW)
    expect(result.workflow?.paused).toBeUndefined()
    expect(result.launches).toEqual([])
  })
})

describe("advanceWorkflow review stage", () => {
  const reviewed = workflow({
    review: { kind: "claude", model: "claude-fable-5" },
    launched: [{ id: "a", at: NOW - 200_000 }],
  })
  const labeled = { ...child("a", "in_progress"), labels: [REVIEW_LABEL] }

  test("launches a reviewer when a launched bead gets the needs-review label", () => {
    const result = advanceWorkflow(reviewed, [labeled, child("b")], new Set(["b"]), dead, NOW)
    expect(result.reviews.map((bead) => bead.id)).toEqual(["a"])
    expect(result.workflow?.paused).toBeUndefined()
  })

  test("a labeled bead with a dead worker is review-bound, not a stall", () => {
    const result = advanceWorkflow(reviewed, [labeled], new Set(), dead, NOW)
    expect(result.workflow?.paused).toBeUndefined()
    expect(result.reviews.map((bead) => bead.id)).toEqual(["a"])
  })

  test("does not relaunch a review already in flight, and it holds the bead's slot", () => {
    const wf = workflow({ ...reviewed, reviews: [{ id: "a", at: NOW - 10_000 }] })
    const aliveReviewer = (binding: string) => binding === reviewBindingId("a")
    const result = advanceWorkflow(wf, [labeled, child("b")], new Set(["b"]), aliveReviewer, NOW)
    expect(result.reviews).toEqual([])
    expect(result.launches).toEqual([])
  })

  test("pauses when a reviewer dies without closing the bead", () => {
    const wf = workflow({ ...reviewed, reviews: [{ id: "a", at: NOW - STALL_GRACE_MS - 1 }] })
    const result = advanceWorkflow(wf, [labeled], new Set(), dead, NOW)
    expect(result.workflow?.paused).toContain("reviewer for a")
  })

  test("ignores the label when the workflow has no review step", () => {
    const wf = workflow({ launched: [{ id: "a", at: NOW - STALL_GRACE_MS - 1 }] })
    const result = advanceWorkflow(wf, [labeled], new Set(), dead, NOW)
    expect(result.reviews).toEqual([])
    expect(result.workflow?.paused).toContain("agent for a")
  })

  test("frees the slot once the reviewer closes the bead", () => {
    const wf = workflow({ ...reviewed, reviews: [{ id: "a", at: NOW - 100_000 }] })
    const result = advanceWorkflow(wf, [child("a", "closed"), child("b")], new Set(["b"]), dead, NOW)
    expect(result.launches.map((bead) => bead.id)).toEqual(["b"])
  })
})

describe("worktrees", () => {
  test("defaults derive a stable branch and sibling path from the epic", () => {
    const defaults = worktreeDefaults("/code/repo", "Epic-1")
    expect(defaults.branch).toBe("workflow/epic-1")
    expect(defaults.path).toBe("/code/repo-epic-1")
    expect(worktreeDefaults("/code/repo", "Epic-1")).toEqual(defaults)
  })

  test("exempt IDs cover only beads launched by worktree'd workflows", () => {
    const isolated = workflow({
      worktree: { path: "/code/repo-epic-1", branch: "workflow/epic-1" },
      launched: [{ id: "a", at: NOW }, { id: "b", at: NOW }],
    })
    const shared = workflow({ epicId: "epic-2", launched: [{ id: "c", at: NOW }] })
    expect([...worktreeExemptIds([isolated, shared])].sort()).toEqual(["a", "b"])
    expect(worktreeExemptIds([shared]).size).toBe(0)
  })
})

describe("resumeWorkflow", () => {
  test("clears the pause and forgets stalled launches so they retry", () => {
    const wf = workflow({
      paused: "agent for b exited without closing the bead",
      launched: [{ id: "a", at: NOW - 100_000 }, { id: "b", at: NOW - 100_000 }],
    })
    const resumed = resumeWorkflow(wf, [child("a", "closed"), child("b", "in_progress")], dead)
    expect(resumed.paused).toBeUndefined()
    expect(resumed.launched.map((launch) => launch.id)).toEqual(["a"])

    const next = advanceWorkflow(resumed, [child("a", "closed"), child("b")], new Set(["b"]), dead, NOW)
    expect(next.launches.map((bead) => bead.id)).toEqual(["b"])
  })

  test("keeps labeled workers, drops dead reviewers so the review retries", () => {
    const labeled = { ...child("a", "in_progress"), labels: [REVIEW_LABEL] }
    const wf = workflow({
      review: { kind: "pi" },
      paused: "reviewer for a exited without closing the bead",
      launched: [{ id: "a", at: NOW - 300_000 }],
      reviews: [{ id: "a", at: NOW - 200_000 }],
    })
    const resumed = resumeWorkflow(wf, [labeled], dead)
    expect(resumed.launched.map((launch) => launch.id)).toEqual(["a"])
    expect(resumed.reviews).toEqual([])

    const next = advanceWorkflow(resumed, [labeled], new Set(), dead, NOW)
    expect(next.reviews.map((bead) => bead.id)).toEqual(["a"])
  })
})
