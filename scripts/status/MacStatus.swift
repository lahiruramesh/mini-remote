import AppKit
import Foundation

final class MiniRemoteStatus: NSObject, NSApplicationDelegate {
    private let connectorLabel = "local.miniremote.connector"
    private var connectorAgent: URL {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let loginAgent = home.appendingPathComponent("Library/LaunchAgents/local.miniremote.connector.plist")
        return FileManager.default.fileExists(atPath: loginAgent.path) ? loginAgent
            : home.appendingPathComponent("Library/Application Support/MiniRemote/local.miniremote.connector.plist")
    }
    private let viewerApp = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Applications/MiniRemote Viewer.app")
    private let logFile = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Logs/MiniRemote/connector.log")

    private var statusItem: NSStatusItem!
    private var statusMenuItem: NSMenuItem!
    private var startStopItem: NSMenuItem!
    private var restartItem: NSMenuItem!
    private var refreshTimer: Timer?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        createStatusItem()
        refreshStatus()
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            self?.refreshStatus()
        }
    }

    private func createStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = statusItem.button {
            let icon = Bundle.main.resourceURL?.appendingPathComponent("mini-remote.png")
            if let icon, let image = NSImage(contentsOf: icon) {
                image.size = NSSize(width: 18, height: 18)
                button.image = image
                button.image?.isTemplate = false
            } else {
                button.title = "MR"
            }
            button.toolTip = "Mini Remote"
        }

        let menu = NSMenu()
        statusMenuItem = NSMenuItem(title: "Mini Remote — checking…", action: nil, keyEquivalent: "")
        statusMenuItem.isEnabled = false
        menu.addItem(statusMenuItem)
        menu.addItem(.separator())

        startStopItem = NSMenuItem(title: "Start Connector", action: #selector(toggleConnector), keyEquivalent: "")
        startStopItem.target = self
        menu.addItem(startStopItem)

        restartItem = NSMenuItem(title: "Restart Connector", action: #selector(restartConnector), keyEquivalent: "")
        restartItem.target = self
        menu.addItem(restartItem)

        menu.addItem(.separator())
        addMenuItem("Open Viewer", action: #selector(openViewerAction))
        addMenuItem("Pair Phone…", action: #selector(pairPhone))
        addMenuItem("Setup…", action: #selector(openSetup))
        addMenuItem("Open Connector Log", action: #selector(openLog))
        menu.addItem(.separator())
        addMenuItem("Quit Status Icon", action: #selector(quitStatusIcon))
        statusItem.menu = menu
    }

    private func addMenuItem(_ title: String, action: Selector) {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        statusItem.menu?.addItem(item)
    }

    private var launchDomain: String { "gui/\(getuid())" }

    private func launchctl(_ arguments: [String]) -> (Int32, String) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = arguments
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        do {
            try process.run()
            let data = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return (process.terminationStatus, String(decoding: data, as: UTF8.self))
        } catch {
            return (127, error.localizedDescription)
        }
    }

    private func connectorIsRunning() -> Bool {
        let (status, output) = launchctl(["print", "\(launchDomain)/\(connectorLabel)"])
        return status == 0 && output.contains("state = running")
    }

    private func refreshStatus() {
        let running = connectorIsRunning()
        statusMenuItem.title = running ? "Mini Remote — Running" : "Mini Remote — Stopped"
        startStopItem.title = running ? "Stop Connector" : "Start Connector"
        restartItem.isEnabled = running
    }

    @objc private func toggleConnector() {
        if connectorIsRunning() {
            _ = launchctl(["bootout", "\(launchDomain)/\(connectorLabel)"])
        } else {
            _ = launchctl(["bootstrap", launchDomain, connectorAgent.path])
        }
        refreshStatus()
    }

    @objc private func restartConnector() {
        if connectorIsRunning() {
            _ = launchctl(["kickstart", "-k", "\(launchDomain)/\(connectorLabel)"])
        } else {
            _ = launchctl(["bootstrap", launchDomain, connectorAgent.path])
        }
        refreshStatus()
    }

    private func launchViewer(arguments: [String] = []) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/open")
        process.arguments = ["-a", viewerApp.path]
        if !arguments.isEmpty {
            process.arguments = ["-n", "-a", viewerApp.path, "--args"] + arguments
        }
        try? process.run()
    }

    @objc private func openViewerAction() { launchViewer() }
    @objc private func pairPhone() { launchViewer(arguments: ["--pair-phone"]) }
    @objc private func openSetup() { launchViewer(arguments: ["--setup"]) }

    @objc private func openLog() {
        NSWorkspace.shared.open(logFile)
    }

    @objc private func quitStatusIcon() {
        NSApp.terminate(nil)
    }
}

let application = NSApplication.shared
let statusApp = MiniRemoteStatus()
application.delegate = statusApp
application.run()
