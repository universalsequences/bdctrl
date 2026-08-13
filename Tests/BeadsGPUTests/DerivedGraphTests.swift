import XCTest
@testable import BeadsGPU

final class DerivedGraphTests: XCTestCase {
    func testReadyBlockedAndMembership() throws {
        let lines = [
          #"{"id":"p","title":"Epic","status":"open","priority":1,"issue_type":"epic"}"#,
          #"{"id":"a","title":"Done blocker","status":"closed","priority":2,"issue_type":"task","dependencies":[{"issue_id":"a","depends_on_id":"p","type":"parent-child"}]}"#,
          #"{"id":"b","title":"Ready","status":"open","priority":2,"issue_type":"task","dependencies":[{"issue_id":"b","depends_on_id":"p","type":"parent-child"},{"issue_id":"b","depends_on_id":"a","type":"blocks"}]}"#,
          #"{"id":"c","title":"Blocked","status":"open","priority":2,"issue_type":"task","dependencies":[{"issue_id":"c","depends_on_id":"b","type":"blocks"}]}"#
        ]
        let issues = try lines.map { try JSONDecoder().decode(Issue.self, from: Data($0.utf8)) }
        let graph = DerivedGraph(issues: issues)
        XCTAssertTrue(graph.readyIDs.contains("b"))
        XCTAssertTrue(graph.blockedIDs.contains("c"))
        XCTAssertEqual(graph.parentByChild["b"], "p")
        XCTAssertEqual(graph.groups.first?.closedCount, 1)
    }
}
