import MetalKit
import simd

private struct ViewUniforms {
    var viewport: SIMD2<Float>
    var center: SIMD2<Float>
    var zoom: Float
    var time: Float
}

// Byte layouts must match the structs in BeadShaders.source.
private struct BeadInstance {
    var center: SIMD2<Float>
    var radius: Float
    var flags: UInt32
    var color: SIMD4<Float>
}

private struct QuadInstance {
    var center: SIMD2<Float>
    var size: SIMD2<Float>
    var palette0: SIMD4<Float>
    var palette1: SIMD4<Float>
    var palette2: SIMD4<Float>
    var palette3: SIMD4<Float>
    var seed: Float
    var flags: UInt32
    var style: UInt32
    var pointStart: UInt32
    var pointCount: UInt32
    var padding0: UInt32 = 0
    var padding1: UInt32 = 0
}

private struct EpicVisualStyle {
    let colors: [SIMD4<Float>]
    let seed: Float
    let shader: UInt32
    let meshSeed: UInt64

    init(epicID: String) {
        // Swift's Hasher is intentionally randomized between launches. FNV-1a
        // gives each epic a stable visual identity without storing preferences.
        var hash: UInt64 = 1469598103934665603
        for byte in epicID.utf8 { hash = (hash ^ UInt64(byte)) &* 1099511628211 }
        let palettes: [[SIMD4<Float>]] = [
            [.init(0.12, 0.32, 0.58, 1), .init(0.18, 0.72, 0.74, 1), .init(0.72, 0.42, 0.86, 1), .init(0.96, 0.72, 0.42, 1)],
            [.init(0.42, 0.10, 0.28, 1), .init(0.92, 0.26, 0.32, 1), .init(1.00, 0.58, 0.28, 1), .init(0.98, 0.84, 0.56, 1)],
            [.init(0.08, 0.25, 0.24, 1), .init(0.18, 0.52, 0.38, 1), .init(0.58, 0.72, 0.36, 1), .init(0.90, 0.82, 0.50, 1)],
            [.init(0.20, 0.15, 0.48, 1), .init(0.42, 0.32, 0.84, 1), .init(0.88, 0.34, 0.68, 1), .init(0.38, 0.78, 0.92, 1)],
            [.init(0.18, 0.24, 0.36, 1), .init(0.40, 0.52, 0.72, 1), .init(0.72, 0.76, 0.86, 1), .init(0.86, 0.58, 0.46, 1)],
            [.init(0.34, 0.12, 0.10, 1), .init(0.64, 0.28, 0.18, 1), .init(0.82, 0.58, 0.32, 1), .init(0.42, 0.66, 0.62, 1)]
        ]
        colors = palettes[Int(hash % UInt64(palettes.count))]
        shader = UInt32((hash >> 8) % 4)
        seed = Float((hash >> 16) & 0xffff) / Float(0xffff)
        meshSeed = hash
    }
}

private struct BlobMeshVertex {
    var position: SIMD2<Float>
    var tone: Float
    var padding0: Float = 0
    var color: SIMD4<Float>
}

private struct WireVertex {
    var position: SIMD2<Float>
    var normal: SIMD2<Float>
    var v: Float
    var along: Float
    var color: SIMD4<Float>
    var dashed: UInt32
    var pad0: UInt32 = 0
    var pad1: UInt32 = 0
    var pad2: UInt32 = 0
}

private struct LabelInstance {
    var anchor: SIMD2<Float>
    var sizePx: SIMD2<Float>
    var uvMin: SIMD2<Float>
    var uvMax: SIMD2<Float>
    var color: SIMD4<Float>
}

private enum BeadFlags {
    static let ready: UInt32 = 1, blocked: UInt32 = 2, closed: UInt32 = 4, inProgress: UInt32 = 8
    static let selected: UInt32 = 16, hovered: UInt32 = 32, faded: UInt32 = 64
    static let pearl: UInt32 = 256
}

@MainActor
final class BeadsRenderer: NSObject, MTKViewDelegate {
    let device: MTLDevice
    private let queue: MTLCommandQueue
    private let beadPipeline: MTLRenderPipelineState
    private let shadowPipeline: MTLRenderPipelineState
    private let wirePipeline: MTLRenderPipelineState
    private let blobMeshPipeline: MTLRenderPipelineState
    private let labelPipeline: MTLRenderPipelineState
    private let labelAtlas: LabelAtlas
    private let linearSampler: MTLSamplerState

    var graph = DerivedGraph.empty {
        didSet {
            graphRevision += 1
            var map: [String: Set<String>] = [:]
            for edge in graph.edges {
                map[edge.issueID, default: []].insert(edge.dependsOnID)
                map[edge.dependsOnID, default: []].insert(edge.issueID)
            }
            adjacency = map
        }
    }
    var layout: ForceLayout?
    var cameraCenter = CGPoint.zero
    var zoom: CGFloat = 1
    var hoveredID: String?
    var selectedID: String?
    var isManipulatingGeometry = false
    private let started = CACurrentMediaTime()

    // Geometry is world-space, so camera moves only touch uniforms. Instance
    // arrays and buffers are rebuilt only when this signature changes.
    private var graphRevision = 0
    private var adjacency: [String: Set<String>] = [:]
    private var cachedSignature = Int.min
    private var cachedBlobs: [QuadInstance] = []
    private var cachedBlobMesh: [BlobMeshVertex] = []
    private var cachedWires: [WireVertex] = []
    private var cachedBeads: [BeadInstance] = []
    private var blobBuffer: MTLBuffer?
    private var pointBuffer: MTLBuffer?
    private var blobMeshBuffer: MTLBuffer?
    private var wireBuffer: MTLBuffer?
    private var beadBuffer: MTLBuffer?
    private var labelBuffer: MTLBuffer?
    private var cachedLabels: [LabelInstance] = []
    private var blobLoopCache: [String: (hash: Int, loops: [[SIMD2<Float>]])] = [:]
    private var blobMeshCache: [String: (hash: Int, triangles: [[BlobMesh.Triangle]])] = [:]

    init?(view: MTKView) {
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else { return nil }
        self.device = device; self.queue = queue
        view.device = device
        view.colorPixelFormat = .bgra8Unorm
        // The visual motion is intentionally slow; 30 Hz is smooth while
        // halving continuous fragment and post-processing work.
        view.preferredFramesPerSecond = 30
        view.enableSetNeedsDisplay = false
        let library: MTLLibrary
        do { library = try device.makeLibrary(source: BeadShaders.source, options: nil) }
        catch { NSLog("beadsgpu shader compile failed: \(error)"); return nil }
        func pipeline(_ vertex: String, _ fragment: String, format: MTLPixelFormat, blend: Bool,
                      premultiplied: Bool = false) -> MTLRenderPipelineState? {
            let d = MTLRenderPipelineDescriptor()
            d.vertexFunction = library.makeFunction(name: vertex)
            d.fragmentFunction = library.makeFunction(name: fragment)
            d.colorAttachments[0].pixelFormat = format
            if blend {
                d.colorAttachments[0].isBlendingEnabled = true
                d.colorAttachments[0].sourceRGBBlendFactor = premultiplied ? .one : .sourceAlpha
                d.colorAttachments[0].destinationRGBBlendFactor = .oneMinusSourceAlpha
                d.colorAttachments[0].sourceAlphaBlendFactor = .one
                d.colorAttachments[0].destinationAlphaBlendFactor = .oneMinusSourceAlpha
            }
            return try? device.makeRenderPipelineState(descriptor: d)
        }
        let directFormat = view.colorPixelFormat
        guard let bead = pipeline("beadVertex", "beadFragment", format: directFormat, blend: true),
              let shadow = pipeline("beadVertex", "beadShadowFragment", format: directFormat, blend: true),
              let wire = pipeline("wireVertex", "wireFragment", format: directFormat, blend: true),
              let blobMesh = pipeline("blobMeshVertex", "blobMeshFragment", format: directFormat, blend: true),
              let label = pipeline("labelVertex", "labelFragment", format: directFormat, blend: true, premultiplied: true),
              let atlas = LabelAtlas(device: device)
        else { return nil }
        beadPipeline = bead; shadowPipeline = shadow; wirePipeline = wire
        blobMeshPipeline = blobMesh; labelPipeline = label; labelAtlas = atlas
        let samplerDescriptor = MTLSamplerDescriptor()
        samplerDescriptor.minFilter = .linear; samplerDescriptor.magFilter = .linear
        samplerDescriptor.sAddressMode = .clampToEdge; samplerDescriptor.tAddressMode = .clampToEdge
        guard let sampler = device.makeSamplerState(descriptor: samplerDescriptor) else { return nil }
        linearSampler = sampler
        super.init()
        view.delegate = self
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        guard let layout, let drawable = view.currentDrawable,
              let pass = view.currentRenderPassDescriptor,
              let command = queue.makeCommandBuffer() else { return }
        layout.step()
        let targetFPS = (layout.isSettling || isManipulatingGeometry) ? 30 : 20
        if view.preferredFramesPerSecond != targetFPS { view.preferredFramesPerSecond = targetFPS }
        let time = Float(CACurrentMediaTime() - started)
        var uniforms = ViewUniforms(viewport: SIMD2(Float(view.drawableSize.width), Float(view.drawableSize.height)),
                                    center: SIMD2(Float(cameraCenter.x), Float(cameraCenter.y)),
                                    zoom: Float(zoom * view.windowBackingScale), time: time)

        // One direct-to-drawable pass. The old HDR, bright extraction, Gaussian
        // blur, and composite passes dominated GPU time on Retina displays.
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].storeAction = .store
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0.022, green: 0.028, blue: 0.042, alpha: 1)
        guard let encoder = command.makeRenderCommandEncoder(descriptor: pass) else { return }
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<ViewUniforms>.stride, index: 1)
        rebuildGeometryIfNeeded(layout)

        if !cachedBlobMesh.isEmpty, let blobMeshBuffer {
            encoder.setRenderPipelineState(blobMeshPipeline)
            encoder.setVertexBuffer(blobMeshBuffer, offset: 0, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: cachedBlobMesh.count)
        }
        if !cachedWires.isEmpty, let wireBuffer {
            encoder.setRenderPipelineState(wirePipeline)
            encoder.setVertexBuffer(wireBuffer, offset: 0, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: cachedWires.count)
        }
        if !cachedBeads.isEmpty, let beadBuffer {
            encoder.setRenderPipelineState(shadowPipeline)
            encoder.setVertexBuffer(beadBuffer, offset: 0, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: cachedBeads.count)
            encoder.setRenderPipelineState(beadPipeline)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: cachedBeads.count)
        }
        if !cachedLabels.isEmpty, let labelBuffer {
            encoder.setRenderPipelineState(labelPipeline)
            encoder.setVertexBuffer(labelBuffer, offset: 0, index: 0)
            encoder.setFragmentTexture(labelAtlas.texture, index: 0)
            encoder.setFragmentSamplerState(linearSampler, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: cachedLabels.count)
        }
        encoder.endEncoding()
        command.present(drawable)
        command.commit()
    }

    // MARK: - Instance building

    private func rebuildGeometryIfNeeded(_ layout: ForceLayout) {
        var hasher = Hasher()
        hasher.combine(layout.revision)
        hasher.combine(graphRevision)
        hasher.combine(hoveredID)
        hasher.combine(selectedID)
        hasher.combine(isManipulatingGeometry)
        let signature = hasher.finalize()
        guard signature != cachedSignature else { return }
        cachedSignature = signature

        let (blobs, blobPoints, blobMesh) = blobInstances(layout)
        cachedBlobs = blobs
        cachedBlobMesh = blobMesh
        cachedWires = edgeGeometry(layout)
        cachedBeads = beadInstances(layout)
        blobBuffer = blobs.isEmpty ? nil
            : device.makeBuffer(bytes: blobs, length: MemoryLayout<QuadInstance>.stride * blobs.count)
        pointBuffer = device.makeBuffer(bytes: blobPoints, length: MemoryLayout<SIMD2<Float>>.stride * blobPoints.count)
        blobMeshBuffer = blobMesh.isEmpty ? nil
            : device.makeBuffer(bytes: blobMesh, length: MemoryLayout<BlobMeshVertex>.stride * blobMesh.count)
        wireBuffer = cachedWires.isEmpty ? nil
            : device.makeBuffer(bytes: cachedWires, length: MemoryLayout<WireVertex>.stride * cachedWires.count)
        beadBuffer = cachedBeads.isEmpty ? nil
            : device.makeBuffer(bytes: cachedBeads, length: MemoryLayout<BeadInstance>.stride * cachedBeads.count)
        cachedLabels = labelInstances(layout)
        labelBuffer = cachedLabels.isEmpty ? nil
            : device.makeBuffer(bytes: cachedLabels, length: MemoryLayout<LabelInstance>.stride * cachedLabels.count)
    }

    private func labelInstances(_ layout: ForceLayout) -> [LabelInstance] {
        var out: [LabelInstance] = []
        for issue in graph.issues.values {
            if issue.issueType == "epic", let group = graph.groups.first(where: { $0.id == issue.id }) {
                guard let entry = labelAtlas.entry(key: "e:\(issue.id)", text: issue.title,
                                                   font: .systemFont(ofSize: 12, weight: .semibold)) else { continue }
                let summit = layout.blobSummit(group)
                out.append(LabelInstance(anchor: SIMD2(Float(summit.x), Float(summit.y)), sizePx: entry.sizePx,
                                         uvMin: entry.uvMin, uvMax: entry.uvMax, color: SIMD4(0.63, 0.72, 0.84, 0.9)))
            } else if let p = layout.positions[issue.id] {
                guard let entry = labelAtlas.entry(key: "n:\(issue.id)", text: issue.id,
                                                   font: .monospacedSystemFont(ofSize: 9, weight: .medium)) else { continue }
                out.append(LabelInstance(anchor: SIMD2(Float(p.x), Float(p.y + 30)), sizePx: entry.sizePx,
                                         uvMin: entry.uvMin, uvMax: entry.uvMax, color: SIMD4(0.72, 0.75, 0.80, 0.78)))
            }
        }
        return out
    }

    private func blobInstances(_ layout: ForceLayout) -> ([QuadInstance], [SIMD2<Float>], [BlobMeshVertex]) {
        var instances: [QuadInstance] = [], points: [SIMD2<Float>] = [], meshVertices: [BlobMeshVertex] = []
        for group in graph.groups {
            let hasReady = !Set(group.childIDs).intersection(graph.readyIDs).isEmpty
            let complete = group.progress >= 1 && !group.childIDs.isEmpty
            let flags: UInt32 = complete ? 2 : (hasReady ? 1 : 0)
            let visual = EpicVisualStyle(epicID: group.id)
            var palette = visual.colors
            if complete {
                let completionTint = SIMD4<Float>(0.34, 0.72, 0.48, 1)
                palette = palette.map { simd_mix($0, completionTint, SIMD4<Float>(repeating: 0.58)) }
            }
            var positionHasher = Hasher()
            for id in group.childIDs + [group.id] {
                guard let p = layout.positions[id] else { continue }
                // Contouring is substantially heavier than bead/wire updates;
                // sub-four-point motion is visually indistinguishable.
                positionHasher.combine(Int((p.x / 4).rounded()))
                positionHasher.combine(Int((p.y / 4).rounded()))
            }
            let positionHash = positionHasher.finalize()
            let loops: [[SIMD2<Float>]]
            if let cached = blobLoopCache[group.id], cached.hash == positionHash {
                loops = cached.loops
            } else {
                loops = BlobContour.loops(field: layout.blobField(group))
                blobLoopCache[group.id] = (positionHash, loops)
            }
            let meshTriangles: [[BlobMesh.Triangle]]
            if layout.isSettling, let cached = blobMeshCache[group.id],
               cached.triangles.count == loops.count {
                // Keep the initial topology stable while forces settle instead
                // of flipping and retriangulating every frame. A contour can
                // occasionally split/merge loops, invalidating cache shape.
                meshTriangles = cached.triangles
            } else if let cached = blobMeshCache[group.id], cached.hash == positionHash,
                      cached.triangles.count == loops.count {
                meshTriangles = cached.triangles
            } else {
                meshTriangles = loops.enumerated().map {
                    BlobMesh.triangles(loop: $0.element, seed: visual.meshSeed &+ UInt64($0.offset))
                }
                blobMeshCache[group.id] = (positionHash, meshTriangles)
            }
            for (loopIndex, loop) in loops.enumerated() {
                var lo = loop[0], hi = loop[0]
                for p in loop { lo = min(lo, p); hi = max(hi, p) }
                lo -= SIMD2(repeating: 36); hi += SIMD2(repeating: 36)
                instances.append(QuadInstance(center: (lo + hi) * 0.5, size: hi - lo,
                                              palette0: palette[0], palette1: palette[1],
                                              palette2: palette[2], palette3: palette[3],
                                              seed: visual.seed, flags: flags, style: visual.shader,
                                              pointStart: UInt32(points.count), pointCount: UInt32(loop.count)))
                points += loop

                let triangles = meshTriangles.indices.contains(loopIndex) ? meshTriangles[loopIndex] : []
                for triangle in triangles {
                    // Color is resolved per vertex from its tone; the GPU
                    // interpolates across the facet, so shading reads as one
                    // continuous gradient over the mesh.
                    for (position, tone) in [(triangle.a, triangle.toneA),
                                             (triangle.b, triangle.toneB),
                                             (triangle.c, triangle.toneC)] {
                        let color = simd_mix(palette[1], palette[3], SIMD4<Float>(repeating: tone * 0.72))
                        meshVertices.append(BlobMeshVertex(position: position, tone: tone, color: color))
                    }
                }
            }
        }
        if points.isEmpty { points.append(.zero) }
        return (instances, points, meshVertices)
    }

    private func beadInstances(_ layout: ForceLayout) -> [BeadInstance] {
        graph.issues.values.filter { issue in issue.issueType != "epic" || !graph.groups.contains(where: { $0.id == issue.id }) }.compactMap { issue in
            guard let p = layout.positions[issue.id] else { return nil }
            let radius = Float(18 + max(0, 4 - issue.priority))
            var color: SIMD4<Float>
            switch issue.issueType {
            case "decision": color = .init(0.52, 0.38, 0.72, 1)
            case "chore": color = .init(0.32, 0.52, 0.52, 1)
            default: color = .init(0.34, 0.46, 0.64, 1)
            }
            var flags: UInt32 = 0
            if issue.issueType == "epic" { flags |= BeadFlags.pearl }
            if graph.readyIDs.contains(issue.id) { flags |= BeadFlags.ready }
            if graph.blockedIDs.contains(issue.id) { flags |= BeadFlags.blocked }
            if issue.status == "closed" { flags |= BeadFlags.closed }
            if issue.status == "in_progress" { flags |= BeadFlags.inProgress }
            if issue.id == selectedID { flags |= BeadFlags.selected }
            if issue.id == hoveredID { flags |= BeadFlags.hovered }
            if let hoveredID, issue.id != hoveredID,
               adjacency[hoveredID]?.contains(issue.id) != true { flags |= BeadFlags.faded }
            return BeadInstance(center: .init(Float(p.x), Float(p.y)), radius: radius, flags: flags, color: color)
        }
    }

    private func edgeGeometry(_ layout: ForceLayout) -> [WireVertex] {
        var wires: [WireVertex] = []
        for edge in graph.edges where edge.type == "blocks" || edge.type == "related" {
            guard let a = layout.positions[edge.dependsOnID], let b = layout.positions[edge.issueID] else { continue }
            let hoverTouches = hoveredID == nil || hoveredID == edge.issueID || hoveredID == edge.dependsOnID
                || graph.parentByChild[edge.issueID] == hoveredID || graph.parentByChild[edge.dependsOnID] == hoveredID
            let related = edge.type == "related"
            let alpha: Float = !hoverTouches ? 0.06 : (related ? 0.16 : 0.42)
            let color = related ? SIMD4<Float>(0.55, 0.60, 0.72, alpha) : SIMD4<Float>(0.84, 0.80, 0.72, alpha)

            // Gentle bowed quadratic between the two beads.
            let a2 = SIMD2(Float(a.x), Float(a.y)), b2 = SIMD2(Float(b.x), Float(b.y))
            let delta = b2 - a2
            let length = max(1, simd_length(delta))
            let normal = SIMD2(-delta.y, delta.x) / length
            let control = (a2 + b2) * 0.5 + normal * min(35, length * 0.14)
            func point(_ t: Float) -> SIMD2<Float> {
                let u = 1 - t
                return a2 * (u * u) + control * (2 * u * t) + b2 * (t * t)
            }

            let segments = 24
            var along: Float = 0
            for i in 0..<segments {
                let p0 = point(Float(i) / Float(segments))
                let p1 = point(Float(i + 1) / Float(segments))
                let d = p1 - p0
                let l = max(0.001, simd_length(d))
                let n = SIMD2(-d.y, d.x) / l
                let nextAlong = along + l
                let quad = [(p0, Float(-1), along), (p0, Float(1), along), (p1, Float(-1), nextAlong),
                            (p1, Float(-1), nextAlong), (p0, Float(1), along), (p1, Float(1), nextAlong)]
                wires += quad.map { WireVertex(position: $0.0, normal: n, v: $0.1, along: $0.2,
                                               color: color, dashed: related ? 1 : 0) }
                along = nextAlong
            }
        }
        return wires
    }
}

private extension MTKView { var windowBackingScale: CGFloat { window?.backingScaleFactor ?? 2 } }
