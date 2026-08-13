import SwiftUI
import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        // SwiftPM produces a plain Mach-O executable rather than an .app
        // bundle. Such processes otherwise inherit the prohibited/background
        // activation policy and their SwiftUI windows never become visible.
        NSApplication.shared.setActivationPolicy(.regular)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.activate(ignoringOtherApps: true)
        NSApplication.shared.windows.first?.makeKeyAndOrderFront(nil)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main
struct BeadsGPUApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model: AppModel

    init() {
        let argument = CommandLine.arguments.dropFirst().first { !$0.hasPrefix("-") }
        let path: String? = argument.map { NSString(string: $0).expandingTildeInPath }
            ?? UserDefaults.standard.string(forKey: "lastProject")
        let url = path.map { URL(fileURLWithPath: $0, isDirectory: true) }
        _model = StateObject(wrappedValue: AppModel(initialURL: url))
    }

    var body: some Scene {
        WindowGroup("beadsgpu") { RootView(model: model).frame(minWidth: 850, minHeight: 560) }
            .defaultSize(width: 1180, height: 760)
            .commands {
                CommandGroup(replacing: .newItem) {
                    Button("Open Project…") { model.chooseProject() }.keyboardShortcut("o")
                }
                CommandGroup(after: .sidebar) {
                    Button("Find Issue…") { model.showFind = true }.keyboardShortcut("f")
                    Button("Refresh") { model.refresh() }.keyboardShortcut("r")
                    Button("Fit Graph") { model.fitGraph() }.keyboardShortcut("0")
                }
            }
    }
}
