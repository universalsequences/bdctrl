import Foundation
import CoreServices

/// Watches the files that represent bead changes, rather than every bit of
/// database housekeeping under `.beads`.
final class DirectoryWatcher: @unchecked Sendable {
    private var stream: FSEventStreamRef?
    private let callback: @Sendable () -> Void

    init(url: URL, callback: @escaping @Sendable () -> Void) {
        self.callback = callback
        var context = FSEventStreamContext(version: 0, info: Unmanaged.passUnretained(self).toOpaque(), retain: nil, release: nil, copyDescription: nil)
        stream = FSEventStreamCreate(nil, { _, info, count, eventPaths, eventFlags, _ in
            guard let info else { return }
            let watcher = Unmanaged<DirectoryWatcher>.fromOpaque(info).takeUnretainedValue()
            let paths = Unmanaged<CFArray>.fromOpaque(eventPaths).takeUnretainedValue() as NSArray

            for index in 0..<count {
                guard DirectoryWatcher.isContentChange(eventFlags[index]),
                      let path = paths[index] as? String,
                      DirectoryWatcher.isBeadsDataPath(path) else { continue }
                watcher.callback()
                return
            }
        }, &context, [url.path] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 0.2,
        FSEventStreamCreateFlags(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagNoDefer))
        if let stream {
            FSEventStreamSetDispatchQueue(stream, DispatchQueue.global(qos: .utility))
            FSEventStreamStart(stream)
        }
    }

    /// `bd export` opens an embedded Dolt database and updates its manifest and
    /// journal bookkeeping even though no issue changed. Current bd releases
    /// update `last-touched` for real mutations. JSONL and SQLite names cover
    /// older backends without watching noisy Dolt internals.
    static func isBeadsDataPath(_ path: String) -> Bool {
        let name = URL(fileURLWithPath: path).lastPathComponent.lowercased()
        if name == "last-touched" || name == "issues.jsonl" { return true }
        return name.hasSuffix(".db") || name.hasSuffix(".db-wal") ||
            name.hasSuffix(".sqlite") || name.hasSuffix(".sqlite-wal") ||
            name.hasSuffix(".sqlite3") || name.hasSuffix(".sqlite3-wal")
    }

    private static func isContentChange(_ flags: FSEventStreamEventFlags) -> Bool {
        let changes = FSEventStreamEventFlags(kFSEventStreamEventFlagItemCreated |
            kFSEventStreamEventFlagItemRemoved |
            kFSEventStreamEventFlagItemRenamed |
            kFSEventStreamEventFlagItemModified)
        return flags & changes != 0
    }

    deinit {
        if let stream { FSEventStreamStop(stream); FSEventStreamInvalidate(stream); FSEventStreamRelease(stream) }
    }
}
