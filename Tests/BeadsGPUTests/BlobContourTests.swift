import XCTest
import simd
@testable import BeadsGPU

final class BlobContourTests: XCTestCase {
    func testSingleCenterProducesOneNearCircularLoop() {
        let field = BlobField(centers: [SIMD2<Float>(0, 0)])
        let loops = BlobContour.loops(field: field)
        XCTAssertEqual(loops.count, 1)
        let loop = loops[0]
        XCTAssertGreaterThan(loop.count, 20)
        for p in loop {
            XCTAssertLessThan(abs(field.distance(p)), 8, "contour point should sit near the zero level set")
        }
    }

    func testTwoNearbyCentersMergeIntoOneLoop() {
        let field = BlobField(centers: [SIMD2<Float>(0, 0), SIMD2<Float>(90, 0)])
        let loops = BlobContour.loops(field: field)
        XCTAssertEqual(loops.count, 1)
        for p in loops[0] { XCTAssertLessThan(abs(field.distance(p)), 8) }
    }

    func testFarApartCentersStayConnectedByTendril() {
        let field = BlobField(centers: [SIMD2<Float>(0, 0), SIMD2<Float>(600, 0)])
        let loops = BlobContour.loops(field: field)
        XCTAssertEqual(loops.count, 1, "an outlier must stretch the enclosure, not detach from it")
        // The midpoint of the tendril lies inside the region.
        XCTAssertLessThan(field.distance(SIMD2<Float>(300, 0)), 0)
    }

    func testLoopsAreClosedAndBounded() {
        let field = BlobField(centers: [SIMD2<Float>(0, 0), SIMD2<Float>(70, 40), SIMD2<Float>(-30, 80), SIMD2<Float>(120, -20)])
        let loops = BlobContour.loops(field: field)
        XCTAssertEqual(loops.count, 1)
        for loop in loops {
            XCTAssertLessThanOrEqual(loop.count, 96)
            // Closed: last point should be near the first (same neighborhood, not identical after smoothing).
            let gap = simd_distance(loop.first!, loop.last!)
            XCTAssertLessThan(gap, 40)
        }
    }
}
