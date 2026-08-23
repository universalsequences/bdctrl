import XCTest
import simd
@testable import BeadsGPU

final class BlobMeshTests: XCTestCase {
    func testTriangulatesAClosedContourDeterministically() {
        let loop: [SIMD2<Float>] = [
            .init(-100, -100), .init(0, -112), .init(100, -100), .init(112, 0),
            .init(100, 100), .init(0, 112), .init(-100, 100), .init(-112, 0)
        ]
        let first = BlobMesh.triangles(loop: loop, seed: 42)
        let second = BlobMesh.triangles(loop: loop, seed: 42)
        XCTAssertGreaterThan(first.count, 12)
        XCTAssertEqual(first, second)
        for triangle in first {
            let centroid = (triangle.a + triangle.b + triangle.c) / Float(3)
            XCTAssertTrue(BlobMesh.contains(centroid, polygon: loop))
        }
    }

    func testConcaveContourDoesNotBridgeItsCutout() {
        // A U shape whose upper-middle region lies outside the polygon.
        let loop: [SIMD2<Float>] = [
            .init(0, 0), .init(180, 0), .init(180, 180), .init(125, 180),
            .init(125, 60), .init(55, 60), .init(55, 180), .init(0, 180)
        ]
        let triangles = BlobMesh.triangles(loop: loop, seed: 9)
        XCTAssertFalse(triangles.isEmpty)
        // Rim triangles may graze a few pixels outside the contour by design;
        // what must never survive is a bridge across the cutout, whose edge
        // midpoints would sit tens of pixels outside the polygon.
        func distanceToBoundary(_ p: SIMD2<Float>) -> Float {
            var distance2 = Float.greatestFiniteMagnitude
            var previous = loop.last!
            for current in loop {
                let edge = current - previous
                let t = max(0, min(1, dot(p - previous, edge) / max(0.0001, dot(edge, edge))))
                distance2 = min(distance2, simd_distance_squared(p, previous + edge * t))
                previous = current
            }
            return sqrt(distance2)
        }
        for triangle in triangles {
            let edgeSamples = [(triangle.a + triangle.b) / Float(2),
                               (triangle.b + triangle.c) / Float(2),
                               (triangle.c + triangle.a) / Float(2)]
            XCTAssertTrue(edgeSamples.allSatisfy {
                BlobMesh.contains($0, polygon: loop) || distanceToBoundary($0) < 4
            })
        }
    }
}
