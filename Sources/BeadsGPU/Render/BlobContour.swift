import CoreGraphics
import simd

/// Extracts smooth closed paths around an epic by contouring its BlobField
/// (marching squares → Chaikin smoothing). Same field as hit-testing, so what
/// you see is exactly what you can click.
enum BlobContour {
    static func loops(field: BlobField, cell: Float = 11, maxPoints: Int = 64) -> [[SIMD2<Float>]] {
        let centers = field.centers
        guard !centers.isEmpty else { return [] }
        var lo = centers[0], hi = centers[0]
        for c in centers { lo = min(lo, c); hi = max(hi, c) }
        let pad = BlobField.nodeRadius + BlobField.smoothing + cell * 2
        lo -= SIMD2(repeating: pad); hi += SIMD2(repeating: pad)
        let nx = Int(ceil((hi.x - lo.x) / cell)) + 1
        let ny = Int(ceil((hi.y - lo.y) / cell)) + 1
        guard nx > 2, ny > 2, nx * ny < 200_000 else { return [] }

        let (values, rowStride) = field.sample(lo: lo, cell: cell, nx: nx, ny: ny)

        // Marching squares. Each crossing point lies on a unique grid edge, so
        // edges double as stable keys for chaining segments into loops.
        func hKey(_ ix: Int, _ iy: Int) -> Int { (iy * nx + ix) * 2 }
        func vKey(_ ix: Int, _ iy: Int) -> Int { (iy * nx + ix) * 2 + 1 }
        var pointForEdge: [Int: SIMD2<Float>] = [:]
        var neighbors: [Int: [Int]] = [:]

        func crossing(_ ix0: Int, _ iy0: Int, _ ix1: Int, _ iy1: Int) -> SIMD2<Float> {
            let va = values[iy0 * rowStride + ix0], vb = values[iy1 * rowStride + ix1]
            let t = va / (va - vb)
            return SIMD2(lo.x + (Float(ix0) + t * Float(ix1 - ix0)) * cell,
                         lo.y + (Float(iy0) + t * Float(iy1 - iy0)) * cell)
        }
        func segment(_ e0: Int, _ p0: SIMD2<Float>, _ e1: Int, _ p1: SIMD2<Float>) {
            pointForEdge[e0] = p0; pointForEdge[e1] = p1
            neighbors[e0, default: []].append(e1)
            neighbors[e1, default: []].append(e0)
        }

        for iy in 0..<(ny - 1) {
            for ix in 0..<(nx - 1) {
                let a = values[iy * rowStride + ix] < 0, b = values[iy * rowStride + ix + 1] < 0
                let c = values[(iy + 1) * rowStride + ix + 1] < 0, d = values[(iy + 1) * rowStride + ix] < 0
                let caseIndex = (a ? 1 : 0) | (b ? 2 : 0) | (c ? 4 : 0) | (d ? 8 : 0)
                guard caseIndex != 0 && caseIndex != 15 else { continue }
                let bottom = hKey(ix, iy), top = hKey(ix, iy + 1)
                let left = vKey(ix, iy), right = vKey(ix + 1, iy)
                lazy var pB = crossing(ix, iy, ix + 1, iy)
                lazy var pT = crossing(ix, iy + 1, ix + 1, iy + 1)
                lazy var pL = crossing(ix, iy, ix, iy + 1)
                lazy var pR = crossing(ix + 1, iy, ix + 1, iy + 1)
                switch caseIndex {
                case 1, 14: segment(left, pL, bottom, pB)
                case 2, 13: segment(bottom, pB, right, pR)
                case 3, 12: segment(left, pL, right, pR)
                case 4, 11: segment(right, pR, top, pT)
                case 6, 9: segment(bottom, pB, top, pT)
                case 7, 8: segment(left, pL, top, pT)
                case 5, 10:
                    let center = (values[iy * rowStride + ix] + values[iy * rowStride + ix + 1]
                        + values[(iy + 1) * rowStride + ix + 1] + values[(iy + 1) * rowStride + ix]) * 0.25
                    let joinA = (caseIndex == 5) == (center < 0)
                    if joinA { segment(left, pL, top, pT); segment(bottom, pB, right, pR) }
                    else { segment(left, pL, bottom, pB); segment(right, pR, top, pT) }
                default: break
                }
            }
        }

        // Chain edge-keys into closed loops.
        var visited = Set<Int>()
        var loops: [[SIMD2<Float>]] = []
        for start in neighbors.keys where !visited.contains(start) {
            var loop: [SIMD2<Float>] = []
            var previous = -1, current = start
            while true {
                visited.insert(current)
                if let p = pointForEdge[current] { loop.append(p) }
                guard let next = neighbors[current]?.first(where: { $0 != previous && ($0 == start || !visited.contains($0)) })
                else { break }
                if next == start { break }
                previous = current; current = next
            }
            if loop.count >= 6 { loops.append(smooth(loop, maxPoints: maxPoints)) }
        }
        return loops
    }

    private static func smooth(_ input: [SIMD2<Float>], maxPoints: Int) -> [SIMD2<Float>] {
        var points = input
        for _ in 0..<2 {
            var next: [SIMD2<Float>] = []
            next.reserveCapacity(points.count * 2)
            for i in 0..<points.count {
                let p = points[i], q = points[(i + 1) % points.count]
                next.append(p * 0.75 + q * 0.25)
                next.append(p * 0.25 + q * 0.75)
            }
            points = next
        }
        if points.count > maxPoints {
            let stride = Float(points.count) / Float(maxPoints)
            points = (0..<maxPoints).map { points[Int(Float($0) * stride)] }
        }
        return points
    }
}
