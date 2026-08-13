import SwiftUI

struct RootView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        ZStack(alignment: .top) {
            if model.projectURL == nil {
                ContentUnavailableView("Open a beads project", systemImage: "point.3.connected.trianglepath.dotted", description: Text("Choose a directory containing .beads"))
                    .overlay(alignment: .bottom) { Button("Open Project…") { model.chooseProject() }.buttonStyle(.borderedProminent).padding(50) }
            } else {
                // The pane overlays the canvas rather than splitting it, so
                // opening it never resizes the Metal view or shifts the viewport.
                ZStack(alignment: .trailing) {
                    MetalCanvas(model: model).frame(minWidth: 480, minHeight: 400)
                    if model.selectedID != nil {
                        SidePane(model: model)
                            .frame(width: 380)
                            .transition(.move(edge: .trailing))
                    }
                }
            }
            if let toast = model.toast {
                Text(toast).font(.callout).padding(.horizontal, 14).padding(.vertical, 9).background(.red.opacity(0.9), in: Capsule()).foregroundStyle(.white).padding(.top, 12)
            }
            if model.showFind { FindOverlay(model: model).padding(.top, 45) }
        }
        .animation(.easeOut(duration: 0.18), value: model.selectedID)
        .toolbar {
            ToolbarItemGroup {
                Button { model.chooseProject() } label: { Image(systemName: "folder") }.help("Open project")
                Button { model.refresh() } label: { Image(systemName: "arrow.clockwise") }.help("Refresh")
                if model.isLoading { ProgressView().controlSize(.small) }
            }
        }
        .onExitCommand { if model.showFind { model.showFind = false } else { model.select(nil) } }
    }
}

private struct FindOverlay: View {
    @ObservedObject var model: AppModel
    @State private var query = ""
    @FocusState private var focused: Bool

    private var results: [Issue] {
        let q = query.lowercased()
        return model.graph.issues.values.filter { q.isEmpty || $0.id.lowercased().contains(q) || $0.title.lowercased().contains(q) }
            .sorted { $0.id < $1.id }.prefix(12).map { $0 }
    }

    var body: some View {
        VStack(spacing: 0) {
            TextField("Find issue by id or title", text: $query)
                .textFieldStyle(.plain).font(.title3).padding(12).focused($focused)
                .onSubmit { if let first = results.first { model.center(on: first.id); model.showFind = false } }
            Divider()
            ForEach(results) { issue in
                Button { model.center(on: issue.id); model.showFind = false } label: {
                    HStack { Text(issue.id).font(.system(.caption, design: .monospaced)).frame(width: 85, alignment: .leading); Text(issue.title).lineLimit(1); Spacer() }.padding(.horizontal, 12).padding(.vertical, 7)
                }.buttonStyle(.plain)
            }
        }.frame(width: 430).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10)).shadow(radius: 18)
         .onAppear { focused = true }
    }
}
