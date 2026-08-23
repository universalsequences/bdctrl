import { describe, expect, test } from "bun:test"
import type { Bead } from "../src/beads"
import { blockingBeadIds, nextQueued, pruneQueue, type QueueEntry } from "../src/queue"

function bead(id: string, status = "open"): Bead {
  return { id, title: id, status, priority: 2, issue_type: "task" }
}

function entry(id: string, overrides: Partial<QueueEntry> = {}): QueueEntry {
  return { cwd: "/repo", id, kind: "claude", at: 0, ...overrides }
}

describe("pruneQueue", () => {
  test("drops beads that were closed or claimed elsewhere", () => {
    const issues = new Map([
      ["a", bead("a")],
      ["b", bead("b", "in_progress")],
      ["c", bead("c", "closed")],
    ])
    expect(pruneQueue([entry("a"), entry("b"), entry("c")], issues).map((item) => item.id)).toEqual(["a"])
  })

  test("keeps entries the issue graph has not loaded yet", () => {
    expect(pruneQueue([entry("a")], new Map()).map((item) => item.id)).toEqual(["a"])
  })
})

describe("nextQueued", () => {
  const queue = [entry("a"), entry("b")]
  const readyIds = new Set(["a", "b"])

  test("launches the head of the queue when nothing is running", () => {
    expect(nextQueued(queue, { readyIds, activeAgents: 0 })?.id).toBe("a")
  })

  test("waits while any agent is still working", () => {
    expect(nextQueued(queue, { readyIds, activeAgents: 1 })).toBeUndefined()
  })

  test("skips queued beads that are not ready yet", () => {
    expect(nextQueued(queue, { readyIds: new Set(["b"]), activeAgents: 0 })?.id).toBe("b")
  })

  test("does nothing when no queued bead is ready", () => {
    expect(nextQueued(queue, { readyIds: new Set(["z"]), activeAgents: 0 })).toBeUndefined()
  })
})

describe("blockingBeadIds", () => {
  test("counts a live agent whose bead is still in progress", () => {
    const blocking = blockingBeadIds({ bindings: ["a"], inProgressIds: ["a"], launching: [] })
    expect([...blocking]).toEqual(["a"])
  })

  test("ignores an agent that outlived its bead", () => {
    // The Herdr agent stays alive after closing its bead; the queue must not
    // wait behind it forever.
    const blocking = blockingBeadIds({ bindings: ["a"], inProgressIds: [], launching: [] })
    expect(blocking.size).toBe(0)
  })

  test("ignores in-progress beads with no agent attached", () => {
    const blocking = blockingBeadIds({ bindings: [], inProgressIds: ["a", "b"], launching: [] })
    expect(blocking.size).toBe(0)
  })

  test("ignores the designer", () => {
    const blocking = blockingBeadIds({ bindings: ["__designer__"], inProgressIds: ["a"], launching: [] })
    expect(blocking.size).toBe(0)
  })

  test("counts a reviewer as its bead, once only", () => {
    const blocking = blockingBeadIds({ bindings: ["a", "review:a"], inProgressIds: ["a"], launching: [] })
    expect([...blocking]).toEqual(["a"])
  })

  test("counts launches in flight even before the bead is in progress", () => {
    const blocking = blockingBeadIds({ bindings: [], inProgressIds: [], launching: ["review:b"] })
    expect([...blocking]).toEqual(["b"])
  })

  test("exempt beads never block, whether live, launching, or in review", () => {
    const blocking = blockingBeadIds({
      bindings: ["a", "review:a"],
      inProgressIds: ["a", "b"],
      launching: ["c"],
      exempt: ["a", "c"],
    })
    expect(blocking.size).toBe(0)
  })

  test("exemption is per bead — other agents still block", () => {
    const blocking = blockingBeadIds({
      bindings: ["a", "b"],
      inProgressIds: ["a", "b"],
      launching: [],
      exempt: ["a"],
    })
    expect([...blocking]).toEqual(["b"])
  })
})
