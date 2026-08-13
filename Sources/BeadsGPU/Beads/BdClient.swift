import Foundation

struct BdError: LocalizedError, Sendable {
    let command: String
    let message: String
    var errorDescription: String? { "\(command): \(message)" }
}

actor BdClient {
    let projectURL: URL

    init(projectURL: URL) { self.projectURL = projectURL }

    private func run(_ arguments: [String]) throws -> Data {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["bd"] + arguments
        process.currentDirectoryURL = projectURL
        let output = Pipe(), errors = Pipe()
        process.standardOutput = output
        process.standardError = errors
        do { try process.run() } catch {
            throw BdError(command: "bd \(arguments.joined(separator: " "))", message: error.localizedDescription)
        }
        // Drain stdout while the child runs; `bd export` can exceed the pipe's
        // capacity, so waiting first would deadlock on larger projects.
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let errorData = errors.fileHandleForReading.readDataToEndOfFile()
        guard process.terminationStatus == 0 else {
            let message = String(data: errorData, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)
            throw BdError(command: "bd \(arguments.joined(separator: " "))", message: message ?? "exit \(process.terminationStatus)")
        }
        return data
    }

    func export() throws -> [Issue] {
        let data = try run(["export"])
        let decoder = JSONDecoder()
        return try data.split(separator: 0x0a).filter { !$0.isEmpty }.map { line in
            do { return try decoder.decode(Issue.self, from: Data(line)) }
            catch { throw BdError(command: "bd export", message: "Invalid issue JSON: \(error.localizedDescription)") }
        }
    }

    func comments(for id: String) throws -> [Comment] {
        if let data = try? run(["comments", id, "--json"]), let comments = decodeComments(data) { return comments }
        let data = try run(["comments", id])
        if let comments = decodeComments(data) { return comments }
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? [] : [Comment(id: "plain", text: text)]
    }

    private func decodeComments(_ data: Data) -> [Comment]? {
        let decoder = JSONDecoder()
        if let result = try? decoder.decode([Comment].self, from: data) { return result }
        let lines = data.split(separator: 0x0a)
        let result = lines.compactMap { try? decoder.decode(Comment.self, from: Data($0)) }
        return result.isEmpty && !lines.isEmpty ? nil : result
    }

    func close(_ id: String) throws { _ = try run(["close", id]) }
    func reopen(_ id: String) throws { _ = try run(["reopen", id]) }
    func setPriority(_ priority: Int, for id: String) throws { _ = try run(["priority", id, String(priority)]) }
    func addComment(_ text: String, to id: String) throws { _ = try run(["comment", id, text]) }
    func addLabel(_ label: String, to id: String) throws { _ = try run(["tag", id, label]) }
    func removeLabel(_ label: String, from id: String) throws { _ = try run(["label", "remove", id, label]) }
    func addBlock(from: String, to: String) throws { _ = try run(["link", from, to, "--type", "blocks"]) }
    func removeDependency(from: String, to: String) throws { _ = try run(["dep", "remove", from, to]) }
}
