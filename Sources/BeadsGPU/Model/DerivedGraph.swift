import Foundation

struct EpicGroup: Identifiable, Sendable {
    let id: String
    let epic: Issue
    let childIDs: [String]
    let closedCount: Int
    var progress: Double { childIDs.isEmpty ? 0 : Double(closedCount) / Double(childIDs.count) }
}

struct DerivedGraph: Sendable {
    var issues: [String: Issue]
    var edges: [Dependency]
    var parentByChild: [String: String]
    var groups: [EpicGroup]
    var readyIDs: Set<String>
    var blockedIDs: Set<String>
    var dependents: [String: [String]]

    static let empty = DerivedGraph(issues: [])

    init(issues values: [Issue]) {
        let issueMap = Dictionary(uniqueKeysWithValues: values.map { ($0.id, $0) })
        issues = issueMap
        var unique = Set<Dependency>()
        for issue in values { unique.formUnion(issue.dependencies) }
        edges = Array(unique)
        var parentMap: [String: String] = [:]
        var dependentMap: [String: [String]] = [:]
        for edge in edges {
            if edge.type == "parent-child" { parentMap[edge.issueID] = edge.dependsOnID }
            if edge.type == "blocks" { dependentMap[edge.dependsOnID, default: []].append(edge.issueID) }
        }
        parentByChild = parentMap
        dependents = dependentMap

        var ready = Set<String>(), blocked = Set<String>()
        for issue in values where issue.status == "open" || issue.status == "in_progress" {
            let blockers = edges.filter { $0.type == "blocks" && $0.issueID == issue.id }
            let hasOpenBlocker = blockers.contains { issueMap[$0.dependsOnID]?.status != "closed" }
            if issue.status == "open" && !hasOpenBlocker { ready.insert(issue.id) }
            if hasOpenBlocker { blocked.insert(issue.id) }
        }
        readyIDs = ready
        blockedIDs = blocked

        groups = values.filter { $0.issueType == "epic" }.map { epic in
            let children = parentMap.compactMap { $0.value == epic.id ? $0.key : nil }.sorted()
            return EpicGroup(id: epic.id, epic: epic, childIDs: children,
                             closedCount: children.filter { issueMap[$0]?.status == "closed" }.count)
        }.sorted { $0.id < $1.id }
    }

    func dependencies(of id: String) -> [String] {
        edges.filter { $0.type == "blocks" && $0.issueID == id }.map(\.dependsOnID)
    }

    var topLevelIDs: [String] {
        issues.values.filter { parentByChild[$0.id] == nil && $0.issueType != "epic" }.map(\.id).sorted()
    }
}
