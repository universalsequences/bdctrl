import MetalKit
import MetalPerformanceShaders
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
    var color: SIMD4<Float>
    var flags: UInt32
    var pointStart: UInt32
    var pointCount: UInt32
    var padding: UInt32 = 0
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
    private let blobPipeline: MTLRenderPipelineState
    private let brightPipeline: MTLRenderPipelineState
    private let compositePipeline: MTLRenderPipelineState
    private let labelPipeline: MTLRenderPipelineState
    private let labelAtlas: LabelAtlas
    private let linearSampler: MTLSamplerState
    private let blur: MPSImageGaussianBlur
    private var sceneTexture: MTLTexture?
    private var bloomTextureA: MTLTexture?
    private var bloomTextureB: MTLTexture?

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
    private let started = CACurrentMediaTime()

    // Geometry is world-space, so camera moves only touch uniforms. Instance
    // arrays and buffers are rebuilt only when this signature changes.
    private var graphRevision = 0
    private var adjacency: [String: Set<String>] = [:]
    private var cachedSignature = Int.min
    private var cachedBlobs: [QuadInstance] = []
    private var cachedWires: [WireVertex] = []
    private var cachedBeads: [BeadInstance] = []
    private var blobBuffer: MTLBuffer?
    private var pointBuffer: MTLBuffer?
    private var wireBuffer: MTLBuffer?
    private var beadBuffer: MTLBuffer?
    private var labelBuffer: MTLBuffer?
    private var cachedLabels: [LabelInstance] = []
    private var blobLoopCache: [String: (hash: Int, loops: [[SIMD2<Float>]])] = [:]

    init?(view: MTKView) {
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else { return nil }
        self.device = device; self.queue = queue
        view.device = device
        view.colorPixelFormat = .bgra8Unorm
        view.preferredFramesPerSecond = 60
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
        guard let bead = pipeline("beadVertex", "beadFragment", format: .rgba16Float, blend: true),
              let shadow = pipeline("beadVertex", "beadShadowFragment", format: .rgba16Float, blend: true),
              let wire = pipeline("wireVertex", "wireFragment", format: .rgba16Float, blend: true),
              let blob = pipeline("blobVertex", "blobFragment", format: .rgba16Float, blend: true),
              let bright = pipeline("fullscreenVertex", "brightFragment", format: .rgba16Float, blend: false),
              let composite = pipeline("fullscreenVertex", "compositeFragment", format: view.colorPixelFormat, blend: false),
              let label = pipeline("labelVertex", "labelFragment", format: view.colorPixelFormat, blend: true, premultiplied: true),
              let atlas = LabelAtlas(device: device)
        else { return nil }
        beadPipeline = bead; shadowPipeline = shadow; wirePipeline = wire; blobPipeline = blob
        brightPipeline = bright; compositePipeline = composite; labelPipeline = label; labelAtlas = atlas
        let samplerDescriptor = MTLSamplerDescriptor()
        samplerDescriptor.minFilter = .linear; samplerDescriptor.magFilter = .linear
        samplerDescriptor.sAddressMode = .clampToEdge; samplerDescriptor.tAddressMode = .clampToEdge
        guard let sampler = device.makeSamplerState(descriptor: samplerDescriptor) else { return nil }
        linearSampler = sampler
        blur = MPSImageGaussianBlur(device: device, sigma: 6)
        blur.edgeMode = .clamp
        super.init()
        view.delegate = self
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) { sceneTexture = nil }

    private func ensureTextures(_ size: CGSize) {
        let w = max(4, Int(size.width)), h = max(4, Int(size.height))
        if let scene = sceneTexture, scene.width == w, scene.height == h { return }
        func make(_ w: Int, _ h: Int, usage: MTLTextureUsage) -> MTLTexture? {
            let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba16Float, width: w, height: h, mipmapped: false)
            d.usage = usage; d.storageMode = .private
            return device.makeTexture(descriptor: d)
        }
        sceneTexture = make(w, h, usage: [.renderTarget, .shaderRead])
        bloomTextureA = make(max(2, w / 2), max(2, h / 2), usage: [.renderTarget, .shaderRead, .shaderWrite])
        bloomTextureB = make(max(2, w / 2), max(2, h / 2), usage: [.renderTarget, .shaderRead, .shaderWrite])
    }

    func draw(in view: MTKView) {
        guard let layout, let drawable = view.currentDrawable, let command = queue.makeCommandBuffer() else { return }
        layout.step()
        ensureTextures(view.drawableSize)
        guard let sceneTexture, let bloomTextureA, let bloomTextureB else { return }
        let time = Float(CACurrentMediaTime() - started)
        var uniforms = ViewUniforms(viewport: SIMD2(Float(view.drawableSize.width), Float(view.drawableSize.height)),
                                    center: SIMD2(Float(cameraCenter.x), Float(cameraCenter.y)),
                                    zoom: Float(zoom * view.windowBackingScale), time: time)

        // Pass 1: scene into HDR texture.
        let scenePass = MTLRenderPassDescriptor()
        scenePass.colorAttachments[0].texture = sceneTexture
        scenePass.colorAttachments[0].loadAction = .clear
        scenePass.colorAttachments[0].storeAction = .store
        scenePass.colorAttachments[0].clearColor = MTLClearColor(red: 0.022, green: 0.028, blue: 0.042, alpha: 1)
        guard let encoder = command.makeRenderCommandEncoder(descriptor: scenePass) else { return }
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<ViewUniforms>.stride, index: 1)

        rebuildGeometryIfNeeded(layout)

        if !cachedBlobs.isEmpty, let blobBuffer, let pointBuffer {
            encoder.setRenderPipelineState(blobPipeline)
            encoder.setVertexBuffer(blobBuffer, offset: 0, index: 0)
            encoder.setFragmentBuffer(pointBuffer, offset: 0, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: cachedBlobs.count)
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
        encoder.endEncoding()

        // Pass 2: bright areas into half-res bloom source.
        let brightPass = MTLRenderPassDescriptor()
        brightPass.colorAttachments[0].texture = bloomTextureA
        brightPass.colorAttachments[0].loadAction = .clear
        brightPass.colorAttachments[0].storeAction = .store
        if let bright = command.makeRenderCommandEncoder(descriptor: brightPass) {
            bright.setRenderPipelineState(brightPipeline)
            bright.setFragmentTexture(sceneTexture, index: 0)
            bright.setFragmentSamplerState(linearSampler, index: 0)
            bright.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
            bright.endEncoding()
        }

        // Pass 3: gaussian blur → bloom.
        blur.encode(commandBuffer: command, sourceTexture: bloomTextureA, destinationTexture: bloomTextureB)

        // Pass 4: composite to the drawable.
        if let viewPass = view.currentRenderPassDescriptor,
           let composite = command.makeRenderCommandEncoder(descriptor: viewPass) {
            composite.setRenderPipelineState(compositePipeline)
            composite.setFragmentTexture(sceneTexture, index: 0)
            composite.setFragmentTexture(bloomTextureB, index: 1)
            composite.setFragmentSamplerState(linearSampler, index: 0)
            composite.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
            if !cachedLabels.isEmpty, let labelBuffer {
                composite.setRenderPipelineState(labelPipeline)
                composite.setVertexBuffer(labelBuffer, offset: 0, index: 0)
                composite.setVertexBytes(&uniforms, length: MemoryLayout<ViewUniforms>.stride, index: 1)
                composite.setFragmentTexture(labelAtlas.texture, index: 0)
                composite.setFragmentSamplerState(linearSampler, index: 0)
                composite.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: cachedLabels.count)
            }
            composite.endEncoding()
        }
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
        let signature = hasher.finalize()
        guard signature != cachedSignature else { return }
        cachedSignature = signature

        let (blobs, blobPoints) = blobInstances(layout)
        cachedBlobs = blobs
        cachedWires = edgeGeometry(layout)
        cachedBeads = beadInstances(layout)
        blobBuffer = blobs.isEmpty ? nil
            : device.makeBuffer(bytes: blobs, length: MemoryLayout<QuadInstance>.stride * blobs.count)
        pointBuffer = device.makeBuffer(bytes: blobPoints, length: MemoryLayout<SIMD2<Float>>.stride * blobPoints.count)
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

    private func blobInstances(_ layout: ForceLayout) -> ([QuadInstance], [SIMD2<Float>]) {
        var instances: [QuadInstance] = [], points: [SIMD2<Float>] = []
        for group in graph.groups {
            let hasReady = !Set(group.childIDs).intersection(graph.readyIDs).isEmpty
            let complete = group.progress >= 1 && !group.childIDs.isEmpty
            let flags: UInt32 = complete ? 2 : (hasReady ? 1 : 0)
            let color = complete ? SIMD4<Float>(0.30, 0.60, 0.42, 1) : SIMD4<Float>(0.38, 0.46, 0.60, 1)
            var positionHasher = Hasher()
            for id in group.childIDs + [group.id] {
                guard let p = layout.positions[id] else { continue }
                positionHasher.combine(Int(p.x.rounded())); positionHasher.combine(Int(p.y.rounded()))
            }
            let positionHash = positionHasher.finalize()
            let loops: [[SIMD2<Float>]]
            if let cached = blobLoopCache[group.id], cached.hash == positionHash {
                loops = cached.loops
            } else {
                loops = BlobContour.loops(field: layout.blobField(group))
                blobLoopCache[group.id] = (positionHash, loops)
            }
            for loop in loops {
                var lo = loop[0], hi = loop[0]
                for p in loop { lo = min(lo, p); hi = max(hi, p) }
                lo -= SIMD2(repeating: 36); hi += SIMD2(repeating: 36)
                instances.append(QuadInstance(center: (lo + hi) * 0.5, size: hi - lo, color: color,
                                              flags: flags, pointStart: UInt32(points.count), pointCount: UInt32(loop.count)))
                points += loop
            }
        }
        if points.isEmpty { points.append(.zero) }
        return (instances, points)
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
