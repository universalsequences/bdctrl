import Foundation

struct Dependency: Codable, Hashable, Sendable, Identifiable {
    var issueID: String
    var dependsOnID: String
    var type: String
    var id: String { "\(issueID)|\(dependsOnID)|\(type)" }

    enum CodingKeys: String, CodingKey {
        case issueID = "issue_id"
        case dependsOnID = "depends_on_id"
        case type
    }
}

struct Issue: Codable, Hashable, Sendable, Identifiable {
    let id: String
    var title: String
    var description: String?
    var design: String?
    var notes: String?
    var acceptanceCriteria: String?
    var status: String
    var priority: Int
    var issueType: String
    var assignee: String?
    var owner: String?
    var labels: [String]
    var dependencies: [Dependency]
    var dependencyCount: Int
    var dependentCount: Int
    var commentCount: Int
    var createdAt: String?
    var updatedAt: String?
    var closedAt: String?
    var closeReason: String?

    enum CodingKeys: String, CodingKey {
        case id, title, description, design, notes, status, priority, assignee, owner, labels, dependencies
        case acceptanceCriteria = "acceptance_criteria"
        case issueType = "issue_type"
        case dependencyCount = "dependency_count"
        case dependentCount = "dependent_count"
        case commentCount = "comment_count"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case closedAt = "closed_at"
        case closeReason = "close_reason"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? "Untitled"
        description = try c.decodeIfPresent(String.self, forKey: .description)
        design = try c.decodeIfPresent(String.self, forKey: .design)
        notes = try c.decodeIfPresent(String.self, forKey: .notes)
        acceptanceCriteria = try c.decodeIfPresent(String.self, forKey: .acceptanceCriteria)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "open"
        priority = try c.decodeIfPresent(Int.self, forKey: .priority) ?? 2
        issueType = try c.decodeIfPresent(String.self, forKey: .issueType) ?? "task"
        assignee = try c.decodeIfPresent(String.self, forKey: .assignee)
        owner = try c.decodeIfPresent(String.self, forKey: .owner)
        labels = try c.decodeIfPresent([String].self, forKey: .labels) ?? []
        dependencies = try c.decodeIfPresent([Dependency].self, forKey: .dependencies) ?? []
        dependencyCount = try c.decodeIfPresent(Int.self, forKey: .dependencyCount) ?? dependencies.count
        dependentCount = try c.decodeIfPresent(Int.self, forKey: .dependentCount) ?? 0
        commentCount = try c.decodeIfPresent(Int.self, forKey: .commentCount) ?? 0
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt)
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt)
        closedAt = try c.decodeIfPresent(String.self, forKey: .closedAt)
        closeReason = try c.decodeIfPresent(String.self, forKey: .closeReason)
    }
}

struct Comment: Decodable, Hashable, Sendable, Identifiable {
    var id: String
    var author: String?
    var text: String
    var createdAt: String?

    enum CodingKeys: String, CodingKey { case id, author, text, body, content; case createdAt = "created_at" }

    init(id: String, author: String? = nil, text: String, createdAt: String? = nil) {
        self.id = id; self.author = author; self.text = text; self.createdAt = createdAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? UUID().uuidString
        author = try? c.decode(String.self, forKey: .author)
        text = (try? c.decode(String.self, forKey: .text))
            ?? (try? c.decode(String.self, forKey: .body))
            ?? (try? c.decode(String.self, forKey: .content)) ?? ""
        createdAt = try? c.decode(String.self, forKey: .createdAt)
    }
}
