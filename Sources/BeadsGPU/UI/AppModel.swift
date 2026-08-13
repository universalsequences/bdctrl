import AppKit
import Combine

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var graph = DerivedGraph.empty
    @Published var selectedID: String?
    @Published var comments: [Comment] = []
    @Published var isLoading = false
    @Published var toast: String?
    @Published var showFind = false
    @Published private(set) var projectURL: URL?

    private(set) var layout: ForceLayout?
    private var client: BdClient?
    private var watcher: DirectoryWatcher?
    private var refreshTask: Task<Void, Never>?
    private var toastTask: Task<Void, Never>?

    var selectedIssue: Issue? { selectedID.flatMap { graph.issues[$0] } }

    init(initialURL: URL?) {
        if let initialURL { openProject(initialURL) }
    }

    func openProject(_ url: URL) {
        let url = url.standardizedFileURL
        projectURL = url
        UserDefaults.standard.set(url.path, forKey: "lastProject")
        client = BdClient(projectURL: url)
        layout = ForceLayout(projectURL: url)
        watcher = DirectoryWatcher(url: url.appendingPathComponent(".beads")) { [weak self] in
            Task { @MainActor [weak self] in self?.scheduleRefresh() }
        }
        refresh()
    }

    func chooseProject() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true; panel.canChooseFiles = false
        panel.prompt = "Open Beads Project"
        if panel.runModal() == .OK, let url = panel.url { openProject(url) }
    }

    func scheduleRefresh() {
        refreshTask?.cancel()
        refreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled else { return }
            self?.refresh()
        }
    }

    func refresh() {
        guard let client else { return }
        isLoading = true
        Task {
            do {
                let issues = try await client.export()
                let updated = DerivedGraph(issues: issues)
                // FSEvents can fire for database bookkeeping caused by the read
                // itself. Avoid publishing identical snapshots and restarting
                // any layout work in a refresh loop.
                if updated.issues != graph.issues || Set(updated.edges) != Set(graph.edges) {
                    graph = updated
                    layout?.setGraph(updated)
                    if let selectedID, updated.issues[selectedID] == nil { self.selectedID = nil }
                }
            } catch { show(error) }
            isLoading = false
        }
    }

    func select(_ id: String?) {
        selectedID = id
        comments = []
        guard let id, let issue = graph.issues[id], issue.commentCount > 0, let client else { return }
        Task {
            do { comments = try await client.comments(for: id) }
            catch { show(error) }
        }
    }

    func toggleClosed() {
        guard let issue = selectedIssue, let client else { return }
        var optimistic = issue
        optimistic.status = issue.status == "closed" ? "open" : "closed"
        replace(optimistic)
        perform {
            if issue.status == "closed" { try await client.reopen(issue.id) }
            else { try await client.close(issue.id) }
        }
    }

    func setPriority(_ value: Int) {
        guard var issue = selectedIssue, let client else { return }
        issue.priority = value; replace(issue)
        let id = issue.id
        perform { try await client.setPriority(value, for: id) }
    }

    func submitComment(_ text: String) {
        guard let issue = selectedIssue, let client, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        comments.append(Comment(id: UUID().uuidString, author: "you", text: text))
        perform { try await client.addComment(text, to: issue.id) }
    }

    func addLabel(_ label: String) {
        guard var issue = selectedIssue, let client, !label.isEmpty else { return }
        issue.labels.append(label); replace(issue)
        let id = issue.id
        perform { try await client.addLabel(label, to: id) }
    }

    func removeLabel(_ label: String) {
        guard var issue = selectedIssue, let client else { return }
        issue.labels.removeAll { $0 == label }; replace(issue)
        let id = issue.id
        perform { try await client.removeLabel(label, from: id) }
    }

    func center(on id: String) {
        select(id)
        NotificationCenter.default.post(name: .centerIssue, object: id)
    }

    func fitGraph() { NotificationCenter.default.post(name: .fitGraph, object: nil) }

    private func replace(_ issue: Issue) {
        var values = Array(graph.issues.values)
        if let index = values.firstIndex(where: { $0.id == issue.id }) { values[index] = issue }
        graph = DerivedGraph(issues: values)
        layout?.setGraph(graph)
    }

    private func perform(_ operation: @escaping @Sendable () async throws -> Void) {
        Task {
            do { try await operation(); refresh() }
            catch { show(error); refresh() }
        }
    }

    private func show(_ error: Error) {
        toast = error.localizedDescription
        toastTask?.cancel()
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(4)); self?.toast = nil
        }
    }
}

extension Notification.Name {
    static let fitGraph = Notification.Name("beadsgpu.fitGraph")
    static let centerIssue = Notification.Name("beadsgpu.centerIssue")
}
