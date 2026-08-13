import SwiftUI
import MetalKit

struct MetalCanvas: NSViewRepresentable {
    @ObservedObject var model: AppModel

    func makeNSView(context: Context) -> MetalCanvasView {
        let view = MetalCanvasView()
        view.model = model
        return view
    }

    func updateNSView(_ view: MetalCanvasView, context: Context) {
        view.model = model
        view.renderer?.graph = model.graph
        view.renderer?.layout = model.layout
        view.renderer?.selectedID = model.selectedID
    }
}

@MainActor
final class MetalCanvasView: MTKView {
    weak var model: AppModel? {
        didSet { renderer?.layout = model?.layout; renderer?.graph = model?.graph ?? .empty }
    }
    fileprivate var renderer: BeadsRenderer?
    private var trackingAreaRef: NSTrackingArea?
    private var drag: DragMode = .none
    private var lastDragWorld = CGPoint.zero
    private let tooltipLabel = NSTextField(labelWithString: "")

    enum DragMode { case none, node(String), epic(String), pan }

    init() {
        super.init(frame: .zero, device: nil)
        renderer = BeadsRenderer(view: self)
        wantsLayer = true
        tooltipLabel.isHidden = true
        tooltipLabel.font = .systemFont(ofSize: 12, weight: .medium)
        tooltipLabel.textColor = .white
        tooltipLabel.backgroundColor = NSColor(calibratedWhite: 0.08, alpha: 0.94)
        tooltipLabel.drawsBackground = true
        tooltipLabel.isBezeled = false
        tooltipLabel.wantsLayer = true; tooltipLabel.layer?.cornerRadius = 5
        addSubview(tooltipLabel)
        NotificationCenter.default.addObserver(self, selector: #selector(fitGraph), name: .fitGraph, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(centerIssue(_:)), name: .centerIssue, object: nil)
    }

    required init(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    deinit { NotificationCenter.default.removeObserver(self) }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingAreaRef { removeTrackingArea(trackingAreaRef) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect], owner: self)
        addTrackingArea(area); trackingAreaRef = area
    }

    private func worldPoint(_ event: NSEvent) -> CGPoint {
        guard let r = renderer else { return .zero }
        let p = convert(event.locationInWindow, from: nil)
        let topY = bounds.height - p.y
        return CGPoint(x: (p.x-bounds.midX)/r.zoom+r.cameraCenter.x, y: (topY-bounds.midY)/r.zoom+r.cameraCenter.y)
    }

    private func hitNode(_ world: CGPoint) -> String? {
        guard let model, let layout = model.layout else { return nil }
        return model.graph.issues.values.filter { issue in
            issue.issueType != "epic" || !model.graph.groups.contains { $0.id == issue.id }
        }.first { issue in
            guard let p = layout.positions[issue.id] else { return false }
            return hypot(world.x-p.x, world.y-p.y) <= 26
        }?.id
    }
    private func hitEpic(_ world: CGPoint) -> String? {
        guard let model, let layout = model.layout else { return nil }
        return model.graph.groups.first { layout.blobContains(world, group: $0) }?.id
    }

    override func mouseMoved(with event: NSEvent) {
        let world = worldPoint(event), id = hitNode(world) ?? hitEpic(world)
        renderer?.hoveredID = id
        if let id, let issue = model?.graph.issues[id] {
            tooltipLabel.stringValue = issue.title; tooltipLabel.sizeToFit()
            tooltipLabel.frame = tooltipLabel.frame.insetBy(dx: -7, dy: -4)
            let p = convert(event.locationInWindow, from: nil)
            tooltipLabel.frame.origin = CGPoint(x: min(bounds.width-tooltipLabel.frame.width-8, p.x+12), y: min(bounds.height-tooltipLabel.frame.height-8, p.y+15))
            tooltipLabel.isHidden = false
        } else { tooltipLabel.isHidden = true }
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        let world = worldPoint(event); lastDragWorld = world
        if let id = hitNode(world) {
            model?.select(id); model?.layout?.pin(id); renderer?.isManipulatingGeometry = true; drag = .node(id)
        } else if let id = hitEpic(world) {
            model?.select(id); model?.layout?.freeze(); renderer?.isManipulatingGeometry = true; drag = .epic(id)
            if event.clickCount == 2 { zoomToEpic(id) }
        } else { model?.select(nil); drag = .pan }
    }

    override func mouseDragged(with event: NSEvent) {
        let world = worldPoint(event)
        switch drag {
        case .node(let id): model?.layout?.move(id, to: world)
        case .epic(let id): model?.layout?.moveGroup(id, delta: CGVector(dx: world.x-lastDragWorld.x, dy: world.y-lastDragWorld.y))
        case .pan:
            renderer?.cameraCenter.x -= world.x-lastDragWorld.x
            renderer?.cameraCenter.y -= world.y-lastDragWorld.y
        case .none: break
        }
        lastDragWorld = world
    }

    override func mouseUp(with event: NSEvent) {
        if case .node(let id) = drag { model?.layout?.unpin(id) }
        if case .epic = drag { model?.layout?.persist() }
        renderer?.isManipulatingGeometry = false
        drag = .none
    }

    override func scrollWheel(with event: NSEvent) {
        guard let renderer else { return }
        // Trackpad scrolling is direct manipulation: moving fingers right/up
        // should move the graph right/up, rather than moving the camera that way.
        renderer.cameraCenter.x -= event.scrollingDeltaX / renderer.zoom
        renderer.cameraCenter.y -= event.scrollingDeltaY / renderer.zoom
    }

    override func magnify(with event: NSEvent) {
        guard let renderer else { return }
        let before = worldPoint(event)
        renderer.zoom = min(4, max(0.15, renderer.zoom * (1 + event.magnification)))
        let after = worldPoint(event)
        renderer.cameraCenter.x += before.x-after.x; renderer.cameraCenter.y += before.y-after.y
    }

    @objc func fitGraph() {
        guard let model, let layout = model.layout, !layout.positions.isEmpty, let renderer else { return }
        let ps = layout.positions.values
        let minX = ps.map(\.x).min()!, maxX = ps.map(\.x).max()!, minY = ps.map(\.y).min()!, maxY = ps.map(\.y).max()!
        renderer.cameraCenter = CGPoint(x: (minX+maxX)/2, y: (minY+maxY)/2)
        renderer.zoom = min(2, max(0.15, min(bounds.width/max(200,maxX-minX+140), bounds.height/max(200,maxY-minY+140))))
    }

    @objc private func centerIssue(_ note: Notification) {
        guard let id = note.object as? String, let p = model?.layout?.positions[id], let renderer else { return }
        renderer.cameraCenter = p; renderer.zoom = max(renderer.zoom, 0.85)
    }

    private func zoomToEpic(_ id: String) {
        guard let model, let group = model.graph.groups.first(where: { $0.id == id }), let layout = model.layout, let renderer else { return }
        let rect = layout.blobBounds(group).insetBy(dx: -30, dy: -30)
        renderer.cameraCenter = CGPoint(x: rect.midX, y: rect.midY)
        renderer.zoom = min(2.5, min(bounds.width/rect.width, bounds.height/rect.height)) * 0.9
    }

    override var acceptsFirstResponder: Bool { true }
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { model?.select(nil) } else { super.keyDown(with: event) }
    }
}
