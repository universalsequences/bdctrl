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
        for triangle in triangles {
            let edgeSamples = [(triangle.a + triangle.b) / Float(2),
                               (triangle.b + triangle.c) / Float(2),
                               (triangle.c + triangle.a) / Float(2)]
            XCTAssertTrue(edgeSamples.allSatisfy { BlobMesh.contains($0, polygon: loop) })
        }
    }
}
