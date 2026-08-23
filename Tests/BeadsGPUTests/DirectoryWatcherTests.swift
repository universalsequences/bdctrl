import XCTest
@testable import BeadsGPU

final class DirectoryWatcherTests: XCTestCase {
    func testOnlyBeadDataFilesTriggerRefresh() {
        XCTAssertTrue(DirectoryWatcher.isBeadsDataPath("/project/.beads/last-touched"))
        XCTAssertTrue(DirectoryWatcher.isBeadsDataPath("/project/.beads/issues.jsonl"))
        XCTAssertTrue(DirectoryWatcher.isBeadsDataPath("/project/.beads/beads.db-wal"))

        XCTAssertFalse(DirectoryWatcher.isBeadsDataPath("/project/.beads/embeddeddolt/db/.dolt/noms/manifest"))
        XCTAssertFalse(DirectoryWatcher.isBeadsDataPath("/project/.beads/embeddeddolt/db/.dolt/noms/journal.idx"))
        XCTAssertFalse(DirectoryWatcher.isBeadsDataPath("/project/.beads/backup/manifest"))
    }
}
