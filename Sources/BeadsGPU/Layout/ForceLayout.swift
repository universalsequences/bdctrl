import AppKit

@MainActor
final class ForceLayout {
    /// Bumped whenever any position changes; cheap dirty-check for renderers.
    private(set) var revision = 0
    private(set) var positions: [String: CGPoint] = [:]
    private var velocities: [String: CGVector] = [:]
    private var pinned = Set<String>()
    private var graph = DerivedGraph.empty
    private let storageURL: URL
    private var saveCounter = 0
    private var settleFrames = 0
    var isSettling: Bool { settleFrames > 0 }
    private var layoutSignature = ""

    init(projectURL: URL) {
        let hash = String(projectURL.standardizedFileURL.path.utf8.reduce(UInt64(1469598103934665603)) { ($0 ^ UInt64($1)) &* 1099511628211 }, radix: 16)
        let directory = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("beadsgpu")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        // v2 discards positions produced by the original unbounded simulation.
        storageURL = directory.appendingPathComponent("\(hash)-v2.json")
        if let data = try? Data(contentsOf: storageURL), let saved = try? JSONDecoder().decode([String: SavedPoint].self, from: data) {
            positions = saved.mapValues { CGPoint(x: $0.x, y: $0.y) }
        }
    }

    func setGraph(_ graph: DerivedGraph) {
        // Status/detail refreshes happen frequently (and bd may touch files while
        // exporting). Only topology changes are allowed to wake the simulation.
        let signature = graph.issues.keys.sorted().joined(separator: ",") + "#" +
            graph.edges.filter { $0.type == "blocks" || $0.type == "parent-child" }.map(\.id).sorted().joined(separator: ",")
        let topologyChanged = signature != layoutSignature
        layoutSignature = signature
        self.graph = graph
        let ids = Set(graph.issues.keys)
        positions = positions.filter { ids.contains($0.key) }
        velocities = velocities.filter { ids.contains($0.key) }
        let newIDs = ids.filter { positions[$0] == nil }
        for (index, id) in graph.issues.keys.sorted().enumerated() where positions[id] == nil {
            let angle = CGFloat(index) * 2.39996
            let radius = 90 + 18 * sqrt(CGFloat(index))
            positions[id] = CGPoint(x: cos(angle) * radius, y: sin(angle) * radius)
            velocities[id] = .zero
        }
        // Seed new children near their epic without discarding warm positions.
        for group in graph.groups {
            guard let center = positions[group.id] else { continue }
            for (i, id) in group.childIDs.enumerated() where newIDs.contains(id) {
                let a = CGFloat(i) / CGFloat(max(1, group.childIDs.count)) * .pi * 2
                let radius = min(105, 48 + 9 * sqrt(CGFloat(group.childIDs.count)))
                positions[id] = CGPoint(x: center.x + cos(a) * radius, y: center.y + sin(a) * radius)
            }
        }
        if topologyChanged { settleFrames = 240 }
        revision += 1
    }

    // Manual placement is authoritative: the first drag freezes all automatic
    // motion, and releasing does not wake it again.
    func freeze() {
        settleFrames = 0
        velocities = velocities.mapValues { _ in .zero }
    }
    func pin(_ id: String) { freeze(); pinned.insert(id); velocities[id] = .zero }
    func unpin(_ id: String) { pinned.remove(id); velocities[id] = .zero; persist() }
    func move(_ id: String, to point: CGPoint) { positions[id] = point; velocities[id] = .zero; revision += 1 }
    func moveGroup(_ epicID: String, delta: CGVector) {
        guard let group = graph.groups.first(where: { $0.id == epicID }) else { return }
        for id in group.childIDs + [epicID] {
            guard let p = positions[id] else { continue }
            positions[id] = CGPoint(x: p.x + delta.dx, y: p.y + delta.dy)
        }
        revision += 1
    }

    func step() {
        let ids = Array(graph.issues.keys)
        guard ids.count > 1, settleFrames > 0 else { return }
        settleFrames -= 1
        var forces = Dictionary(uniqueKeysWithValues: ids.map { ($0, CGVector.zero) })
        func body(_ id: String) -> String { graph.parentByChild[id] ?? id }
        func bodyRadius(_ id: String) -> CGFloat {
            guard let group = graph.groups.first(where: { $0.id == id }), let anchor = positions[id] else { return 28 }
            return max(72, group.childIDs.compactMap { positions[$0] }.map { hypot($0.x-anchor.x, $0.y-anchor.y) + 42 }.max() ?? 72)
        }

        // Siblings repel one another, while epic groups repel only as rigid
        // bodies. Child-to-child repulsion across groups was what inflated the
        // entire canvas in the original simulation.
        for i in 0..<ids.count {
            for j in (i + 1)..<ids.count {
                let left = ids[i], right = ids[j], leftBody = body(left), rightBody = body(right)
                guard let a = positions[left], let b = positions[right] else { continue }
                var dx = a.x-b.x, dy = a.y-b.y
                var distance = hypot(dx, dy)
                if distance < 0.01 { dx = 1; dy = 0; distance = 1 }
                dx /= distance; dy /= distance

                if leftBody == rightBody {
                    guard left != leftBody, right != rightBody else { continue }
                    let desired: CGFloat = 58
                    guard distance < desired else { continue }
                    let strength = (desired-distance) * 0.004
                    forces[left]!.dx += dx*strength; forces[left]!.dy += dy*strength
                    forces[right]!.dx -= dx*strength; forces[right]!.dy -= dy*strength
                } else {
                    // Only the body anchors participate between groups.
                    guard left == leftBody, right == rightBody else { continue }
                    let desired = bodyRadius(leftBody) + bodyRadius(rightBody) + 42
                    guard distance < desired else { continue }
                    let strength = (desired-distance) * 0.0025
                    forces[left]!.dx += dx*strength; forces[left]!.dy += dy*strength
                    forces[right]!.dx -= dx*strength; forces[right]!.dy -= dy*strength
                }
            }
        }

        // Dependency springs stay inside an epic. Cross-epic dependencies pull
        // the two rigid body anchors together instead of tearing children out
        // of their blobs. Aggregate duplicate body-to-body edges.
        var crossPairs = Set<String>()
        for edge in graph.edges where edge.type == "blocks" {
            let targetBody = body(edge.issueID), sourceBody = body(edge.dependsOnID)
            if targetBody == sourceBody {
                guard let target = positions[edge.issueID], let source = positions[edge.dependsOnID] else { continue }
                let desired = CGPoint(x: source.x+82, y: source.y)
                forces[edge.issueID]!.dx += (desired.x-target.x)*0.0025
                forces[edge.issueID]!.dy += (desired.y-target.y)*0.002
            } else {
                let key = "\(sourceBody)|\(targetBody)"
                guard crossPairs.insert(key).inserted,
                      let target = positions[targetBody], let source = positions[sourceBody] else { continue }
                let separation = bodyRadius(sourceBody) + bodyRadius(targetBody) + 100
                let desired = CGPoint(x: source.x+separation, y: source.y)
                let fx = (desired.x-target.x)*0.0012, fy = (desired.y-target.y)*0.001
                forces[targetBody]!.dx += fx; forces[targetBody]!.dy += fy
                forces[sourceBody]!.dx -= fx; forces[sourceBody]!.dy -= fy
            }
        }

        // Compact each blob around its fixed body anchor.
        for group in graph.groups {
            guard let center = positions[group.id] else { continue }
            for id in group.childIDs where positions[id] != nil {
                let p = positions[id]!
                forces[id]!.dx += (center.x-p.x)*0.003
                forces[id]!.dy += (center.y-p.y)*0.003
            }
        }

        // A mild tether balances disconnected body repulsion.
        let bodies = Set(ids.map(body))
        for id in bodies where positions[id] != nil {
            let p = positions[id]!
            forces[id]!.dx -= p.x*0.00018
            forces[id]!.dy -= p.y*0.00018
        }

        var maxSpeed: CGFloat = 0
        revision += 1
        for id in ids where !pinned.contains(id) {
            var v = velocities[id] ?? .zero
            v.dx = (v.dx+forces[id]!.dx)*0.70
            v.dy = (v.dy+forces[id]!.dy)*0.70
            let speed = hypot(v.dx, v.dy)
            if speed > 3 { v.dx *= 3/speed; v.dy *= 3/speed }
            maxSpeed = max(maxSpeed, hypot(v.dx, v.dy))
            if let p = positions[id] { positions[id] = CGPoint(x: p.x+v.dx, y: p.y+v.dy) }
            velocities[id] = v
        }

        // Stop entirely once settled (or after four seconds). The map should
        // feel alive while arranging, not breathe forever.
        if maxSpeed < 0.006 || settleFrames == 0 {
            settleFrames = 0
            velocities = velocities.mapValues { _ in .zero }
            persist()
        }
        saveCounter += 1
    }

    func blobBounds(_ group: EpicGroup, padding: CGFloat = 54) -> CGRect {
        let points = group.childIDs.compactMap { positions[$0] }
        guard !points.isEmpty else {
            let c = positions[group.id] ?? .zero
            return CGRect(x: c.x - 80, y: c.y - 55, width: 160, height: 110)
        }
        var rect = CGRect(origin: points[0], size: .zero)
        for p in points.dropFirst() { rect = rect.union(CGRect(origin: p, size: .zero)) }
        return rect.insetBy(dx: -padding, dy: -padding)
    }

    private var fieldCache: [String: (revision: Int, field: BlobField)] = [:]

    func blobDistance(_ point: CGPoint, group: EpicGroup) -> CGFloat {
        CGFloat(blobField(group).distance(SIMD2(Float(point.x), Float(point.y))))
    }

    func blobField(_ group: EpicGroup) -> BlobField {
        if let cached = fieldCache[group.id], cached.revision == revision { return cached.field }
        let centers = group.childIDs.compactMap { positions[$0] }
        let effective = Array((centers.isEmpty ? [positions[group.id] ?? .zero] : centers).prefix(64))
        let field = BlobField(centers: effective.map { SIMD2(Float($0.x), Float($0.y)) })
        fieldCache[group.id] = (revision, field)
        return field
    }

    func blobContains(_ point: CGPoint, group: EpicGroup) -> Bool { blobDistance(point, group: group) < 0 }

    func blobSummit(_ group: EpicGroup) -> CGPoint {
        let field = blobField(group)
        let bounds = blobBounds(group)
        var best = CGPoint(x: bounds.midX, y: bounds.midY), bestDistance = Float.greatestFiniteMagnitude
        for y in 0...16 { for x in 0...20 {
            let p = CGPoint(x: bounds.minX + bounds.width * CGFloat(x) / 20, y: bounds.minY + bounds.height * CGFloat(y) / 16)
            let d = field.distance(SIMD2(Float(p.x), Float(p.y)))
            if d < bestDistance { best = p; bestDistance = d }
        }}
        return best
    }

    func persist() {
        let saved = positions.mapValues { SavedPoint(x: $0.x, y: $0.y) }
        if let data = try? JSONEncoder().encode(saved) { try? data.write(to: storageURL, options: .atomic) }
    }
}

private struct SavedPoint: Codable { let x: Double; let y: Double }
