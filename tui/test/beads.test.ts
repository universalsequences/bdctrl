import { describe, expect, test } from "bun:test"
import { age, matchesBead, parseBeads, parseExport } from "../src/beads"
import { agentName, designerPrompt, herdrError, taskPrompt } from "../src/herdr"

const bead = {
  id: "demo-12",
  title: "Build viewer",
  status: "open",
  priority: 1,
  issue_type: "task",
  created_at: "2025-01-02T00:00:00Z",
}

describe("beads", () => {
  test("parses and orders bd JSON newest first", () => {
    const result = parseBeads(JSON.stringify([
      bead,
      { ...bead, id: "new", created_at: "2025-02-01T00:00:00Z" },
    ]))
    expect(result.map((item) => item.id)).toEqual(["new", "demo-12"])
  })

  test("parses newline-delimited graph exports", () => {
    const result = parseExport([
      JSON.stringify({ ...bead, dependencies: [] }),
      JSON.stringify({ ...bead, id: "demo-epic", issue_type: "epic", dependencies: [] }),
    ].join("\n"))
    expect(result.map((item) => item.id)).toEqual(["demo-12", "demo-epic"])
  })

  test("matches beads across fields, epic, and multiple terms", () => {
    const rich = { ...bead, description: "Wire up Jaki drum patterns", labels: ["audio"], assignee: "alec" }
    const epic = { ...bead, id: "demo-epic", title: "Sequencer epic", issue_type: "epic" }
    expect(matchesBead(rich, "")).toBe(true)
    expect(matchesBead(rich, "  ")).toBe(true)
    expect(matchesBead(rich, "JAKI")).toBe(true)
    expect(matchesBead(rich, "viewer")).toBe(true)
    expect(matchesBead(rich, "demo-12")).toBe(true)
    expect(matchesBead(rich, "audio")).toBe(true)
    expect(matchesBead(rich, "jaki viewer")).toBe(true)
    expect(matchesBead(rich, "jaki missing")).toBe(false)
    expect(matchesBead(rich, "sequencer")).toBe(false)
    expect(matchesBead(rich, "sequencer", epic)).toBe(true)
    expect(matchesBead(bead, "jaki")).toBe(false)
  })

  test("formats age", () => {
    expect(age("2025-01-01T00:00:00Z", Date.parse("2025-01-03T00:00:00Z"))).toBe("2d old")
  })

  test("builds safe Herdr agent data", () => {
    expect(agentName("12/A Weird ID", 1_000)).toMatch(/^bead-12-a-weird-id-/)
    expect(taskPrompt(bead)).toContain("bd update demo-12 --claim")
    expect(taskPrompt(bead)).toContain("create a git commit")
  })

  test("parses herdr error responses", () => {
    const busy = herdrError('{"error":{"code":"agent_pane_busy","message":"pane w1:p1 is not an available shell"},"id":"cli:agent:start"}')
    expect(busy?.code).toBe("agent_pane_busy")
    expect(herdrError('{"id":"cli:agent:start","result":{}}')).toBeUndefined()
    expect(herdrError("not json")).toBeUndefined()
  })

  test("builds designer agent data", () => {
    expect(agentName("designer", 1_000)).toMatch(/^designer-/)
    expect(designerPrompt()).toContain("bd create")
    expect(designerPrompt()).toContain("never implement")
  })
})
