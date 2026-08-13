import SwiftUI

struct SidePane: View {
    @ObservedObject var model: AppModel
    @State private var comment = ""
    @State private var newLabel = ""

    var body: some View {
        if let issue = model.selectedIssue {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    header(issue)
                    markdownSection("Description", issue.description)
                    markdownSection("Design", issue.design)
                    markdownSection("Notes", issue.notes)
                    markdownSection("Acceptance criteria", issue.acceptanceCriteria)
                    properties(issue)
                    links(issue)
                    if issue.issueType == "epic" { children(issue) }
                    comments
                    actions(issue)
                }.padding(18)
            }
            .frame(minWidth: 320, idealWidth: 380, maxWidth: 440)
            .background(Color(nsColor: .controlBackgroundColor))
        }
    }

    private func header(_ issue: Issue) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(issue.id).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary)
            Text(issue.title).font(.title2.weight(.semibold)).textSelection(.enabled)
            HStack { Chip(issue.issueType); Chip(issue.status); Chip("P\(issue.priority)") }
        }
    }

    @ViewBuilder private func markdownSection(_ title: String, _ value: String?) -> some View {
        if let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            DisclosureGroup(title) {
                Text((try? AttributedString(markdown: value)) ?? AttributedString(value))
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6).textSelection(.enabled)
            }.font(.body)
        }
    }

    private func properties(_ issue: Issue) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Properties").font(.headline)
            if let assignee = issue.assignee { LabeledContent("Assignee", value: assignee) }
            if let owner = issue.owner { LabeledContent("Owner", value: owner) }
            if let created = issue.createdAt { LabeledContent("Created", value: shortDate(created)) }
            if let updated = issue.updatedAt { LabeledContent("Updated", value: shortDate(updated)) }
            FlowLayout(spacing: 5) {
                ForEach(issue.labels, id: \.self) { label in
                    HStack(spacing: 3) { Text(label); Button { model.removeLabel(label) } label: { Image(systemName: "xmark") }.buttonStyle(.plain) }
                        .padding(.horizontal, 7).padding(.vertical, 3).background(.quaternary, in: Capsule())
                }
                HStack {
                    TextField("add label", text: $newLabel).textFieldStyle(.plain).frame(width: 75)
                    Button("+") { model.addLabel(newLabel); newLabel = "" }.buttonStyle(.plain)
                }.padding(.horizontal, 7).padding(.vertical, 3).overlay(Capsule().stroke(.quaternary))
            }
        }
    }

    private func links(_ issue: Issue) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Dependencies").font(.headline)
            if model.graph.dependencies(of: issue.id).isEmpty { Text("None").foregroundStyle(.secondary) }
            FlowLayout(spacing: 5) {
                ForEach(model.graph.dependencies(of: issue.id), id: \.self) { id in Button(id) { model.center(on: id) }.buttonStyle(.bordered) }
            }
            if let ids = model.graph.dependents[issue.id], !ids.isEmpty {
                Text("Dependents").font(.subheadline.weight(.semibold))
                FlowLayout(spacing: 5) { ForEach(ids, id: \.self) { id in Button(id) { model.center(on: id) }.buttonStyle(.bordered) } }
            }
        }
    }

    private func children(_ issue: Issue) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            let group = model.graph.groups.first { $0.id == issue.id }
            Text("Children · \(Int((group?.progress ?? 0) * 100))%").font(.headline)
            ForEach(group?.childIDs ?? [], id: \.self) { id in
                Button { model.center(on: id) } label: {
                    HStack { Text(id).font(.system(.caption, design: .monospaced)); Text(model.graph.issues[id]?.title ?? id).lineLimit(1); Spacer() }
                }.buttonStyle(.plain)
            }
        }
    }

    private var comments: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Comments").font(.headline)
            ForEach(model.comments) { item in
                VStack(alignment: .leading, spacing: 3) {
                    HStack { Text(item.author ?? "unknown").font(.caption.weight(.semibold)); Spacer(); Text(shortDate(item.createdAt ?? "")).font(.caption2).foregroundStyle(.secondary) }
                    Text(item.text).textSelection(.enabled)
                }.padding(9).background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 7))
            }
            TextEditor(text: $comment).frame(minHeight: 65).overlay(RoundedRectangle(cornerRadius: 5).stroke(.quaternary))
            HStack { Spacer(); Button("Comment ⌘↩") { sendComment() }.keyboardShortcut(.return, modifiers: .command).disabled(comment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
        }
    }

    private func actions(_ issue: Issue) -> some View {
        HStack {
            Button(issue.status == "closed" ? "Reopen" : "Close") { model.toggleClosed() }.buttonStyle(.borderedProminent)
            Spacer()
            Stepper("Priority \(issue.priority)", value: Binding(get: { issue.priority }, set: { model.setPriority($0) }), in: 0...4)
        }.padding(.top, 4)
    }

    private func sendComment() { let value = comment; comment = ""; model.submitComment(value) }
    private func shortDate(_ value: String) -> String { String(value.prefix(16)).replacingOccurrences(of: "T", with: " ") }
}

private struct Chip: View {
    let text: String
    init(_ text: String) { self.text = text.replacingOccurrences(of: "_", with: " ") }
    var body: some View { Text(text).font(.caption.weight(.medium)).padding(.horizontal, 7).padding(.vertical, 3).background(.quaternary, in: Capsule()) }
}

private struct FlowLayout: Layout {
    var spacing: CGFloat
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? 300; var x: CGFloat = 0, y: CGFloat = 0, row: CGFloat = 0
        for view in subviews { let s = view.sizeThatFits(.unspecified); if x+s.width > width && x > 0 { x=0; y += row+spacing; row=0 }; x += s.width+spacing; row=max(row,s.height) }
        return CGSize(width: width, height: y+row)
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x=bounds.minX, y=bounds.minY, row:CGFloat=0
        for view in subviews { let s=view.sizeThatFits(.unspecified); if x+s.width > bounds.maxX && x > bounds.minX { x=bounds.minX; y += row+spacing; row=0 }; view.place(at: CGPoint(x:x,y:y), proposal: ProposedViewSize(s)); x += s.width+spacing; row=max(row,s.height) }
    }
}
