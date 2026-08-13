import simd

/// The signed distance field an epic's enclosure is built from. Circles around
/// each member are smooth-unioned with capsule "tendrils" along the members'
/// Euclidean minimum spanning tree, so the region is connected by construction:
/// dragging a bead away stretches a neck out to it instead of detaching an
/// island. Rendering (BlobContour) and hit-testing (ForceLayout) both use this
/// type, so the shell you see is exactly the region you can click.
struct BlobField {
    static let nodeRadius: Float = 58
    static let tendrilRadius: Float = 30
    static let smoothing: Float = 28

    let centers: [SIMD2<Float>]
    private let segments: [(a: SIMD2<Float>, b: SIMD2<Float>)]

    init(centers: [SIMD2<Float>]) {
        self.centers = centers
        var segments: [(a: SIMD2<Float>, b: SIMD2<Float>)] = []
        if centers.count > 1 {
            var inTree = [0]
            var outside = Set(1..<centers.count)
            while !outside.isEmpty {
                var best = (i: 0, j: -1, d: Float.greatestFiniteMagnitude)
                for i in inTree {
                    for j in outside {
                        let d = simd_distance_squared(centers[i], centers[j])
                        if d < best.d { best = (i, j, d) }
                    }
                }
                segments.append((centers[best.i], centers[best.j]))
                inTree.append(best.j)
                outside.remove(best.j)
            }
        }
        self.segments = segments
    }

    /// Samples the field over a grid, 8 lanes at a time. Rows are padded to a
    /// multiple of 8 (`rowStride`); padding lanes repeat the last column.
    func sample(lo: SIMD2<Float>, cell: Float, nx: Int, ny: Int) -> (values: [Float], rowStride: Int) {
        let lanes = (nx + 7) / 8
        let rowStride = lanes * 8
        var xs = [SIMD8<Float>](repeating: .zero, count: lanes)
        for l in 0..<lanes {
            var v = SIMD8<Float>()
            for e in 0..<8 { v[e] = lo.x + Float(min(l * 8 + e, nx - 1)) * cell }
            xs[l] = v
        }
        var rows = [SIMD8<Float>](repeating: SIMD8<Float>(repeating: 1e9), count: lanes * ny)
        rows.withUnsafeMutableBufferPointer { buf in
            for c in centers {
                let cx = SIMD8<Float>(repeating: c.x)
                for iy in 0..<ny {
                    let y = lo.y + Float(iy) * cell
                    let dy2 = SIMD8<Float>(repeating: (y - c.y) * (y - c.y))
                    let row = iy * lanes
                    for l in 0..<lanes {
                        let dx = xs[l] - cx
                        let nd = (dx * dx + dy2).squareRoot() - Self.nodeRadius
                        buf[row + l] = Self.smin(buf[row + l], nd)
                    }
                }
            }
            for s in segments {
                let ab = s.b - s.a
                let inverseLength2 = 1 / max(1e-6, simd_length_squared(ab))
                let ax = SIMD8<Float>(repeating: s.a.x), ay = SIMD8<Float>(repeating: s.a.y)
                let abx = SIMD8<Float>(repeating: ab.x), aby = SIMD8<Float>(repeating: ab.y)
                for iy in 0..<ny {
                    let py = SIMD8<Float>(repeating: lo.y + Float(iy) * cell)
                    let row = iy * lanes
                    for l in 0..<lanes {
                        let px = xs[l]
                        let t = (((px - ax) * abx + (py - ay) * aby) * inverseLength2)
                            .clamped(lowerBound: SIMD8<Float>(), upperBound: SIMD8<Float>(repeating: 1))
                        let qx = ax + abx * t - px, qy = ay + aby * t - py
                        let nd = (qx * qx + qy * qy).squareRoot() - Self.tendrilRadius
                        buf[row + l] = Self.smin(buf[row + l], nd)
                    }
                }
            }
        }
        var values = [Float](repeating: 0, count: rowStride * ny)
        rows.withUnsafeBytes { src in
            values.withUnsafeMutableBytes { dst in dst.copyMemory(from: src) }
        }
        return (values, rowStride)
    }

    @inline(__always)
    private static func smin(_ a: SIMD8<Float>, _ b: SIMD8<Float>) -> SIMD8<Float> {
        let k = smoothing
        let h = (0.5 + 0.5 * (b - a) / k)
            .clamped(lowerBound: SIMD8<Float>(), upperBound: SIMD8<Float>(repeating: 1))
        return b * (1 - h) + a * h - k * (h * (1 - h))
    }

    func distance(_ p: SIMD2<Float>) -> Float {
        func smin(_ a: Float, _ b: Float) -> Float {
            let k = Self.smoothing
            let h = max(0, min(1, 0.5 + 0.5 * (b - a) / k))
            return b * (1 - h) + a * h - k * h * (1 - h)
        }
        var d = simd_distance(p, centers[0]) - Self.nodeRadius
        for c in centers.dropFirst() { d = smin(d, simd_distance(p, c) - Self.nodeRadius) }
        for s in segments {
            let ab = s.b - s.a
            let t = max(0, min(1, dot(p - s.a, ab) / max(1e-6, dot(ab, ab))))
            d = smin(d, simd_distance(p, s.a + ab * t) - Self.tendrilRadius)
        }
        return d
    }
}
