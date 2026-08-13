# beadsgpu — a Metal-native beads DAG viewer

A macOS app that renders a beads issue database as a spatial dependency graph:
epics drawn as organic SDF "blobs" enclosing their child issues, blocking edges
between nodes, and a SwiftUI side pane for issue details. Canvas machinery is
lifted from `~/code/swift/patch-editor`; issue data comes from the `bd` CLI.

## Goals

- Replace the bdui list view (`localhost:3000/#/issues`) as the daily driver for
  looking at a beads project.
- Answer at a glance: what are the epics, what blocks what, and **what is ready
  to work on right now**.
- Click an issue → side pane with the full detail view (title, description,
  design, notes, acceptance criteria, comments, labels, deps) and basic edits.
- Feel like the patch editor: Metal canvas, smooth pan/zoom, things move
  organically.

Non-goals (v1): multi-project tabs, issue creation UI, epic membership editing,
anything bdui does that isn't listed above. `bd` in a terminal remains the write
path for everything the side pane doesn't cover.

## Data

### Source of truth: the `bd` CLI, not `.beads/issues.jsonl`

The on-disk JSONL lags the live database (observed: 19 issues in the file, 40
from `bd export`). All reads shell out to `bd`:

- **Full snapshot**: `bd export` (run with cwd = target project, e.g. `~/code/…/eseq`).
  One JSON object per line. Fields observed in the wild:
  `id, title, description, design, notes, acceptance_criteria, status, priority,
  issue_type (task|epic|decision|chore|…), assignee, owner, labels?, dependencies,
  dependency_count, dependent_count, comment_count, created_at, updated_at,
  closed_at, close_reason, …`. Parse with `Codable`, unknown fields ignored.
- **Comments**: not inlined in export for all issues — fetch lazily when the
  side pane opens an issue with `comment_count > 0`, via `bd comments <id>` (use
  `--json` if available, else parse; verify flag at build time).

### Graph semantics

`dependencies` is a list of `{issue_id, depends_on_id, type}` edges. Three types
matter:

| type           | meaning                       | rendering                     |
|----------------|-------------------------------|-------------------------------|
| `parent-child` | epic membership               | node lives inside epic's blob |
| `blocks`       | hard dependency (DAG edges)   | directed edge, drives layout  |
| `related`      | soft link                     | faint dashed edge, no layout effect |

Derived per-node state:

- **ready**: `status == open` and every `blocks` dependency is closed → the
  "work on this now" highlight.
- **blocked**: open with at least one open `blocks` dep.
- Epic **progress**: closed children / total children — drives blob styling.

Issues with no `parent-child` edge (orphans, or epics themselves) live outside
any blob at the top level.

### Refresh

- FSEvents watch on the project's `.beads/` directory → debounce (~500 ms) →
  re-run `bd export` → diff against current model → animate changes in
  (new nodes fade/spring in, closed issues transition style; layout warm-starts
  from existing positions so the graph doesn't jump).
- After any write the app itself performs, refresh immediately.
- Manual refresh: `⌘R`.

### Writes (side pane actions → `bd` commands)

- Status: `bd close <id>` / `bd reopen <id>` / `bd update <id> --status …`
- Priority: `bd priority <id> <n>`
- Comment: `bd comment <id> <text>`
- Labels: `bd tag <id> <label>` / `bd label …`
- Dependencies: `bd link <from> <to> --type blocks` (add), `bd dep remove …`

All writes run async off the main thread; UI shows optimistic state, reconciled
by the post-write refresh. Errors surface as a transient HUD toast.

### Project selection

v1: open a project directory (`NSOpenPanel` or `beadsgpu <path>` from the CLI);
remember the last one in `UserDefaults`. Everything else is derived from that
cwd when invoking `bd`.

## Layout

Two nested layout problems, both solved with a force simulation (not Sugiyama —
organic blobs want organic layout, and n≈40 makes force cheap):

(Note: the patch editor ships a full layered-DAG auto-layout —
`Sources/Engine/Geometry/SugiyamaLayout.swift`, 1159 lines, cycle breaking +
crossing minimization + orthogonal routing. It's a clean lift and a good
fallback / initial-placement seed, but the primary look is force-directed:
blobs want organic shapes, and Sugiyama's grid would fight that.)

1. **Within an epic**: children positioned by forces —
   - spring along `blocks` edges with a horizontal bias (dependency flows
     left→right, so "ready" work tends to sit on the left rim of the blob),
   - pairwise repulsion between siblings,
   - weak centering pull toward the epic's local origin.
2. **Between groups**: each epic (and each top-level loose node) is a rigid
   body: blob-vs-blob repulsion using their bounding circles plus padding, so
   blobs never merge visually; springs along aggregated cross-epic edges pull
   related epics near each other.

Simulation runs continuously at low gain (patch-editor style — the graph
breathes), with strong damping so it settles. Node drag pins a node while
dragging; dragging a blob's interior (not a node) drags the whole epic group.
Positions persist per-project in
`~/Library/Application Support/beadsgpu/<hash>.json` so the map is stable
across launches; the simulation warm-starts from saved positions.

## Rendering

Single `MTKView`, layered draw order:

1. **Blob pass** (one instanced quad per epic, fragment shader):
   - SDF = smooth-min (`smin`, polynomial k) of one circle per child node,
     radius ≈ node radius + padding (~28 pt world space). Node positions +
     count fed via a small uniform/storage buffer (cap ~64 nodes per epic;
     beyond that, smin over cluster centroids).
   - Fill: `d < 0` → very subtle dark wash (barely above background).
   - **Shell only, no contour lines**: stroke where `abs(d) < w`, anti-aliased
     with `smoothstep` over `fwidth(d)`.
   - Shell color encodes epic state: default gray-blue; slow soft pulse
     (~0.15 Hz brightness sine) when the epic contains ≥1 ready issue; green
     tint when all children closed.
2. **Edge pass**: `blocks` edges as directed curves (quadratic bezier, slight
   bow) with a small arrowhead or gradient (dim at source → bright at target)
   to show direction; `related` edges faint and dashed. Cross-blob edges route
   blob-boundary to blob-boundary (trim the curve where the SDF crosses zero).
3. **Node pass**: instanced SDF circles/rounded-rects.
   - Fill by `issue_type` (task / decision / chore / epic-loose), ring by
     status: ready = warm glow (the one loud color on screen), blocked = dim,
     in_progress = animated ring, closed = hollow + dimmed.
   - Priority → subtle size delta.
4. **Text pass**: node ID (e.g. `mods.3`) under each node, epic label at the
   blob's **summit** (interior point of maximum SDF distance — always inside,
   unlike the centroid). Title appears as a tooltip-style label on hover.
   Reuse the patch editor's glyph-atlas text renderer.

Camera: world-space transform with pan (drag empty space / two-finger scroll)
and zoom-to-cursor (pinch / scroll-wheel), lifted from the patch editor.
`⌘0` fits the whole graph; double-click a blob zooms to fit that epic.

## Interaction

- **Hover** node → title label + highlight its direct deps/dependents (dim the
  rest ~40%). Hover shell → highlight that epic's cross-epic edges.
- **Click node** → select, open/refresh side pane with issue detail.
- **Click blob interior** (not a node) → select epic → side pane shows epic
  detail + child list (click-through to children).
- **Click empty space** → deselect, side pane collapses.
- **Drag**: node moves node (pinned during drag), blob interior moves the whole
  epic, empty space pans.
- **Keys**: `⌘R` refresh, `⌘0` fit, `esc` deselect, `⌘F` fuzzy-find by
  id/title → select + center.
- Hit-testing: nodes by radius test; blobs by evaluating the same smin SDF on
  CPU (one function, shared constants with the shader).

## Side pane (SwiftUI)

Right-hand pane (~380 pt, slide-in), `NSHostingView` beside the `MTKView` in an
`NSSplitView` — mirrors the bdui modal's content:

- Header: id, title, type/status/priority chips.
- Sections (each collapsible, hidden when empty): description, design, notes,
  acceptance criteria — rendered as markdown (`AttributedString`).
- Properties: assignee, labels (add/remove), created/updated.
- Dependencies / dependents as clickable chips (click → select + center that
  node on canvas).
- Comments: lazy-loaded list + composer (`⌘⏎` submits via `bd comment`).
- Actions: close / reopen button, priority stepper.

Selection state is the bridge: canvas writes selection to an
`@Observable` app model; SwiftUI reacts. Side-pane writes call the `bd` layer
and the refresh loop closes the cycle.

## App structure

SwiftPM executable target (matching the patch editor's build style):

```
Sources/BeadsGPU/
  App/            AppDelegate/main, window + NSSplitView(MTKView | NSHostingView)
  Model/          Issue, Edge, Epic (Codable), DerivedGraph (ready/blocked, progress)
  Beads/          BdClient (Process wrapper: export/comments/writes), FSEvents watcher
  Layout/         force simulation (nodes + rigid epic groups), persistence
  Render/         renderer, camera, passes (blob/edge/node/text), Shaders.metal
  UI/             SwiftUI side pane, app model (@Observable), find overlay
```

Lifted from `~/code/swift/patch-editor` (exact files TBD from survey — see
Appendix A): camera/viewport math, MTKView setup + render loop, glyph-atlas
text rendering, node hit-testing/selection patterns, SDF shader helpers.
Copy-and-trim into this repo rather than depending on the patch-editor package —
the audio engine (DGen/Engine/Jaki) stays behind.

## Milestones

1. **M0 — data**: `BdClient.export()` → `DerivedGraph` printed to stdout for
   eseq. Proves parsing + semantics (ready/blocked/epic membership).
2. **M1 — canvas**: window + camera + instanced node circles + straight edges,
   static force layout, hover/click/drag. No blobs, no text.
3. **M2 — blobs**: smin SDF shell shader, epic grouping forces, blob drag +
   hit-test, epic labels at summit.
4. **M3 — side pane**: SwiftUI detail view, selection bridge, comments lazy
   load, close/reopen/comment writes.
5. **M4 — polish**: FSEvents live refresh with animated diffs, ready-glow +
   pulse, `⌘F` find, position persistence, fit/zoom niceties.

## Appendix A — what to lift from `~/code/swift/patch-editor`

Survey conclusions (paths relative to the patch-editor repo). The app there is
pure AppKit (`main.swift` + `AppDelegate`, `MetalPatchView: MTKView` pinned to
the window's content view); we keep that shape and add the SwiftUI pane via
`NSHostingView`, which the repo already does for a few panels
(`ColorSchemeEditor`, `SourceCodePanel` are SwiftUI hosted in AppKit).

### Clean lifts (grep-verified ~zero audio coupling — copy nearly verbatim)

- **Geometry math** — `Sources/Engine/Geometry/*`: pure `simd`/Foundation.
  Take `ViewportGeometry.swift` (screenToWorld/worldToScreen/zoomToFit),
  `NodeGeometry`, `CableGeometry` (bezier routing + distance queries, reuse for
  edge hit-testing), `HitTestGeometry`, `SelectionGeometry`, `BoundsGeometry`,
  `SugiyamaLayout.swift`. Vendor as a small `Geometry` library target.
- **Camera** — `Sources/PatchEditor/Mouse/CameraHandler.swift` (zoom/pan state,
  `getEffectiveZoomLevel`), `CameraAnimator.swift` (eased panTo/panToNodes),
  `ScrollHandler.swift` (magnify + momentum scroll; delete its effects-pane
  branch). The single `ViewTransform {zoomLevel, panOffset}` uniform in
  `Shaders/Common/ShaderTypes.h` feeds every shader — keep that convention.
- **Text stack** — `GlyphAtlas.swift` (CoreText → atlas texture, on-demand
  rasterization), `GlyphTextRenderer.swift` (instanced glyph quads, per-range
  coloring), `CursorRenderer.swift` (only if in-canvas editing ever happens),
  `Metal/GlyphBuffers.swift`, `Shaders/Core/TextShaders.metal`. Skip the
  legacy `TextRenderer.swift` (per-string textures).
- **Hit-testing** — `HitDetection.swift` (node/cable position queries with
  cache); add one method for blob SDF evaluation.
- **Shaders** — `Shaders/Common/ShaderCommon.h`: `sdf_circle`,
  `sdf_rounded_rect`, noise-perturbed `smooth_sdf_*` variants, SDF-gradient
  lighting, and the **dual-SDF zoom-independent border trick** (outer minus
  `fwidth`-shrunk inner — keeps 1 px shells crisp at any zoom; use it for the
  epic shell stroke). Plus `Core/NodeShaders.metal` (body/outline/glow/pulse
  states), `Core/CableShaders.metal` (bezier SDF + animated flow dots — flow
  dots are a nice "blocked-by" direction cue), `Core/Background.metal`
  (zoom-faded grid/dot background), `UI/SelectionIndicator.metal`.
- **Frame loop & buffer patterns** — `Metal/Draw.swift` (draw-order/passes,
  idle frame-skip), `Metal/Buffers.swift` (instanced buffer building +
  visibility culling; 1 audio ref to strip), `Metal/RenderOptimizations.swift`,
  `Metal/ModularShaderUtils.swift` (concatenates `.metal` sources from disk at
  launch via `makeLibrary(source:)` — keeps shaders hot-reloadable; keep this,
  it's ideal for iterating on the blob shader).
- **AppKit chrome patterns** — `Toast.swift` (HUD toasts for `bd` write
  errors), `CommandSearchPanel.swift` shape for the `⌘F` find overlay,
  `SidebarManager`'s constraint-animated slide-in for the side pane.

### Rebuild, don't port

- **`MetalRenderer.swift`** (1948 lines): the `setupMetal()` pipeline creation
  and MTKView config are the template, but the class owns `Project`/`VM`/audio
  meters — write a slim `BeadsRenderer` from its skeleton instead of gutting it.
- **The data seam**: `Buffers.swift` builders read `PatchGraph`/`PatchNode`,
  which carry operator instances and DSP state. Define `IssueGraph`/`IssueNode`
  exposing the same geometric surface (`position`, `size`, `name`, `zIndex`,
  selection/hover flags) and re-point the builders. **This is the bulk of the
  porting work.**
- **Instance structs** (e.g. `NodeOutlineInstance`, ~30 fields, many
  audio-specific): prune hard, but Swift and Metal struct layouts must stay
  byte-identical — prune both sides together (footgun documented in the repo's
  `TEXT_RENDERING.md`).
- **Interaction handlers** (`CanvasMouseHandler` + `Selection/Drag/Hover`
  handlers): the dispatch structure is right but they thread `vm`/`project`
  through `CanvasContext`; rewrite against the issue-graph model, which also
  lets them shrink a lot (no wires-from-iolets, no resize, no text editing).

### Leave behind

`DGen`, `Jaki`, `Engine/VM|operators|Agents|Audio*`, `audiograph` (also drop
`Package.swift`'s `unsafeFlags -L ./audiograph` link), all of
`Shaders/Operators/*` (Knob/Scope/PianoRoll widgets), `KeyboardHandler.swift`
(1450-line dispatcher — our key map is ~5 bindings), presentation mode,
effect pane, agent chat, sample/schema browsers.
