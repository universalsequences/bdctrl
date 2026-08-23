import simd

/// Builds the irregular triangulation drawn inside an epic contour. Boundary
/// samples are intentionally much denser than interior samples, producing the
/// small rim triangles and broad central facets characteristic of the reference.
enum BlobMesh {
    struct Triangle: Equatable {
        let a: SIMD2<Float>
        let b: SIMD2<Float>
        let c: SIMD2<Float>
        /// Per-vertex tones. Shared vertices carry identical tones across all
        /// incident triangles, so interpolated shading is continuous over the
        /// whole mesh instead of stepping per facet.
        let toneA: Float
        let toneB: Float
        let toneC: Float
    }

    /// Spacing of resampled boundary points; rim triangles are roughly this size.
    private static let boundarySpacing: Float = 9
    /// Exclusion radius at the boundary and its exponential growth length.
    private static let rimRadius: Float = 8
    private static let radiusFalloff: Float = 42
    private static let maxRadius: Float = 64

    static func triangles(loop: [SIMD2<Float>], seed: UInt64, maxPoints: Int = 320) -> [Triangle] {
        guard loop.count >= 3 else { return [] }
        var lo = loop[0], hi = loop[0]
        for p in loop { lo = min(lo, p); hi = max(hi, p) }
        let extent = hi - lo
        guard extent.x > 1, extent.y > 1 else { return [] }

        var rng = SplitMix64(seed: seed)
        // A dense, jittered boundary gives the small rim triangles their scale;
        // the coarse contour loop alone would cap rim density at its own spacing.
        var points = resample(loop, spacing: boundarySpacing, rng: &rng)
        let boundaryCount = points.count
        var interior: [(point: SIMD2<Float>, radius: Float)] = []
        let attempts = min(14_000, max(2_000, maxPoints * 40))

        // Variable-radius dart throwing: the exclusion radius grows exponentially
        // with distance from the boundary, so points pack tightly along the rim
        // and thin out into broad central facets. Uniform proposals rarely land
        // in the thin rim band, so most darts are aimed inward from a random
        // boundary point with an exponentially distributed inset.
        for _ in 0..<attempts where interior.count < maxPoints {
            var p: SIMD2<Float>
            if rng.unitFloat() < 0.7 {
                let at = points[Int(rng.unitFloat() * Float(boundaryCount)) % boundaryCount]
                let angle = rng.unitFloat() * 2 * Float.pi
                let inset = -radiusFalloff * 1.3 * log(max(1e-6, 1 - rng.unitFloat()))
                p = at + SIMD2(cos(angle), sin(angle)) * (5 + inset)
            } else {
                p = lo + SIMD2(rng.unitFloat() * extent.x, rng.unitFloat() * extent.y)
            }
            guard contains(p, polygon: loop) else { continue }
            let edgeDistance = distanceToBoundary(p, polygon: loop)
            guard edgeDistance > boundarySpacing * 0.55 else { continue }
            let radius = min(maxRadius, rimRadius * exp(edgeDistance / radiusFalloff))
            var accepted = true
            for sample in interior {
                let separation = max(radius, sample.radius)
                if simd_distance_squared(p, sample.point) < separation * separation {
                    accepted = false
                    break
                }
            }
            if accepted {
                interior.append((p, radius))
                points.append(p)
            }
        }

        // Tone per point, not per triangle: bright along the dense rim and
        // falling off smoothly with depth, plus a little hash noise so the
        // gradient stays organic. Shared vertices make the shading continuous.
        var tones = [Float](repeating: 0, count: points.count)
        for index in points.indices {
            let depth = index < boundaryCount ? 0 : distanceToBoundary(points[index], polygon: loop)
            var noise = SplitMix64(seed: seed &+ UInt64(index) &* 0x9e3779b97f4a7c15)
            tones[index] = min(1, exp(-depth / (radiusFalloff * 1.4)) * 0.88 + noise.unitFloat() * 0.16)
        }

        let indexed = delaunay(points)
        var result: [Triangle] = []
        result.reserveCapacity(indexed.count)
        for triangle in indexed {
            let a = points[triangle.a], b = points[triangle.b], c = points[triangle.c]
            guard abs(cross(b - a, c - a)) > 0.01 else { continue }
            // An unconstrained Delaunay edge can bridge a concavity. Sampling
            // each edge plus the centroid removes those triangles. Rim
            // triangles legitimately graze just outside the smoothed polygon,
            // so allow a few pixels of tolerance — only samples well outside
            // the contour (true concavity bridges) cull the triangle.
            let centroid: SIMD2<Float> = (a + b + c) / Float(3)
            let ab: SIMD2<Float> = (a + b) / Float(2)
            let bc: SIMD2<Float> = (b + c) / Float(2)
            let ca: SIMD2<Float> = (c + a) / Float(2)
            let samples: [SIMD2<Float>] = [centroid, ab, bc, ca,
                                           a * 0.75 + b * 0.25,
                                           b * 0.75 + c * 0.25,
                                           c * 0.75 + a * 0.25]
            let insideEnough: (SIMD2<Float>) -> Bool = {
                contains($0, polygon: loop) || distanceToBoundary($0, polygon: loop) < 3.5
            }
            guard samples.allSatisfy(insideEnough) else { continue }
            result.append(Triangle(a: a, b: b, c: c,
                                   toneA: tones[triangle.a],
                                   toneB: tones[triangle.b],
                                   toneC: tones[triangle.c]))
        }
        return result
    }

    static func contains(_ point: SIMD2<Float>, polygon: [SIMD2<Float>]) -> Bool {
        guard polygon.count >= 3 else { return false }
        var inside = false
        var j = polygon.count - 1
        for i in polygon.indices {
            let a = polygon[i], b = polygon[j]
            let edge = b - a, relative = point - a
            let projection = dot(relative, edge)
            if abs(cross(edge, relative)) < 0.001 && projection >= 0 && projection <= dot(edge, edge) {
                return true // The contour itself belongs to the filled region.
            }
            if ((a.y > point.y) != (b.y > point.y)) &&
                point.x < (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x {
                inside.toggle()
            }
            j = i
        }
        return inside
    }

    /// Arc-length resampling of the contour loop with per-step jitter, so rim
    /// triangle edges vary in length instead of reading as a regular fringe.
    private static func resample(_ loop: [SIMD2<Float>], spacing: Float,
                                 rng: inout SplitMix64) -> [SIMD2<Float>] {
        var perimeter: Float = 0
        var previous = loop.last!
        for p in loop { perimeter += simd_distance(previous, p); previous = p }
        guard perimeter > spacing * 3 else { return loop }

        var result: [SIMD2<Float>] = []
        var next = spacing * (0.6 + 0.8 * rng.unitFloat())
        var travelled: Float = 0
        previous = loop.last!
        for p in loop {
            let length = simd_distance(previous, p)
            while travelled + length >= next {
                let t = (next - travelled) / max(1e-5, length)
                result.append(previous + (p - previous) * t)
                next += spacing * (0.6 + 0.8 * rng.unitFloat())
            }
            travelled += length
            previous = p
        }
        return result.count >= 3 ? result : loop
    }

    private struct IndexTriangle { let a: Int; let b: Int; let c: Int }
    private struct Edge: Hashable {
        let a: Int
        let b: Int
        init(_ x: Int, _ y: Int) { a = min(x, y); b = max(x, y) }
    }

    /// Bowyer-Watson Delaunay triangulation. Blob meshes are tiny and rebuilt
    /// only after meaningful layout movement, so the straightforward O(n²)
    /// implementation is preferable to another dependency.
    private static func delaunay(_ input: [SIMD2<Float>]) -> [IndexTriangle] {
        guard input.count >= 3 else { return [] }
        var points = input
        var lo = input[0], hi = input[0]
        for p in input { lo = min(lo, p); hi = max(hi, p) }
        let center = (lo + hi) / 2
        let span = max(hi.x - lo.x, hi.y - lo.y) * 16 + 1
        let superStart = points.count
        points += [center + SIMD2(-2 * span, span), center + SIMD2(0, -2 * span), center + SIMD2(2 * span, span)]
        var triangles = [IndexTriangle(a: superStart, b: superStart + 1, c: superStart + 2)]

        for pointIndex in input.indices {
            var bad: [Int] = []
            for (index, triangle) in triangles.enumerated() where circumcircleContains(points[pointIndex], triangle, points) {
                bad.append(index)
            }
            var edgeCounts: [Edge: Int] = [:]
            for index in bad {
                let t = triangles[index]
                edgeCounts[Edge(t.a, t.b), default: 0] += 1
                edgeCounts[Edge(t.b, t.c), default: 0] += 1
                edgeCounts[Edge(t.c, t.a), default: 0] += 1
            }
            let badSet = Set(bad)
            triangles = triangles.enumerated().compactMap { badSet.contains($0.offset) ? nil : $0.element }
            for (edge, count) in edgeCounts where count == 1 {
                triangles.append(IndexTriangle(a: edge.a, b: edge.b, c: pointIndex))
            }
        }
        return triangles.filter { $0.a < input.count && $0.b < input.count && $0.c < input.count }
    }

    private static func circumcircleContains(_ p: SIMD2<Float>, _ t: IndexTriangle,
                                             _ points: [SIMD2<Float>]) -> Bool {
        let a = points[t.a] - p, b = points[t.b] - p, c = points[t.c] - p
        let determinant = (a.x * a.x + a.y * a.y) * cross(b, c)
            - (b.x * b.x + b.y * b.y) * cross(a, c)
            + (c.x * c.x + c.y * c.y) * cross(a, b)
        let orientation = cross(points[t.b] - points[t.a], points[t.c] - points[t.a])
        return orientation > 0 ? determinant > 0.0001 : determinant < -0.0001
    }

    private static func distanceToBoundary(_ p: SIMD2<Float>, polygon: [SIMD2<Float>]) -> Float {
        var distance2 = Float.greatestFiniteMagnitude
        var previous = polygon.last!
        for current in polygon {
            let edge = current - previous
            let t = max(0, min(1, dot(p - previous, edge) / max(0.0001, dot(edge, edge))))
            distance2 = min(distance2, simd_distance_squared(p, previous + edge * t))
            previous = current
        }
        return sqrt(distance2)
    }

    private static func cross(_ a: SIMD2<Float>, _ b: SIMD2<Float>) -> Float { a.x * b.y - a.y * b.x }
}

private struct SplitMix64 {
    var state: UInt64
    init(seed: UInt64) { state = seed }

    mutating func next() -> UInt64 {
        state &+= 0x9e3779b97f4a7c15
        var z = state
        z = (z ^ (z >> 30)) &* 0xbf58476d1ce4e5b9
        z = (z ^ (z >> 27)) &* 0x94d049bb133111eb
        return z ^ (z >> 31)
    }

    mutating func unitFloat() -> Float {
        Float(next() >> 40) / Float(1 << 24)
    }
}
