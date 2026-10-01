import AppKit
import Foundation
import UserNotifications

let center = UNUserNotificationCenter.current()
let output = DispatchQueue(label: "gh-overview.notifier.output")
var queue: [[String: Any]] = []
var busy = false
var started = 0
var closing = false
var lastState: String?

func option(_ name: String) -> String? {
    CommandLine.arguments.first { $0.hasPrefix("--\(name)=") }.map { String($0.dropFirst(name.count + 3)) }
}

func flag(_ name: String) -> Bool {
    CommandLine.arguments.contains("--\(name)")
}

func emit(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object),
          let line = String(data: data, encoding: .utf8)
    else { return }
    output.sync { FileHandle.standardOutput.write(Data((line + "\n").utf8)) }
}

func report(_ status: UNAuthorizationStatus) {
    let state: String
    switch status {
    case .authorized, .provisional: state = "authorized"
    case .notDetermined: state = "pending"
    default: state = "denied"
    }
    guard state != lastState else { return }
    lastState = state
    emit(["event": "ready", "state": state])
}

func refreshAuthorization() {
    center.getNotificationSettings { settings in
        DispatchQueue.main.async { report(settings.authorizationStatus) }
    }
}

func removeThread(_ key: String, then done: @escaping () -> Void) {
    center.getPendingNotificationRequests { pending in
        center.removePendingNotificationRequests(
            withIdentifiers: pending.filter { $0.content.threadIdentifier == key }.map(\.identifier)
        )
        center.getDeliveredNotifications { delivered in
            center.removeDeliveredNotifications(
                withIdentifiers: delivered.filter { $0.request.content.threadIdentifier == key }.map(\.request.identifier)
            )
            DispatchQueue.main.async(execute: done)
        }
    }
}

func show(_ command: [String: Any], then done: @escaping () -> Void) {
    guard let key = command["key"] as? String else { return done() }
    let generation = command["generation"] as? Int ?? 0
    let content = UNMutableNotificationContent()
    content.title = command["title"] as? String ?? ""
    content.subtitle = command["subtitle"] as? String ?? ""
    content.body = command["message"] as? String ?? ""
    content.sound = .default
    content.threadIdentifier = key
    content.categoryIdentifier = (command["snoozable"] as? Bool ?? false) ? "snoozable" : "oneshot"
    content.userInfo = ["key": key, "generation": generation]
    removeThread(key) {
        let request = UNNotificationRequest(identifier: "\(key)#\(generation)", content: content, trigger: nil)
        center.add(request) { error in
            if let error {
                emit(["event": "error", "key": key, "message": error.localizedDescription])
            }
            DispatchQueue.main.async(execute: done)
        }
    }
}

func handle(_ command: [String: Any], then done: @escaping () -> Void) {
    switch command["op"] as? String {
    case "show":
        show(command, then: done)
    case "remove":
        guard let key = command["key"] as? String else { return done() }
        removeThread(key, then: done)
    case "clear":
        center.removeAllDeliveredNotifications()
        center.removeAllPendingNotificationRequests()
        done()
    default:
        emit(["event": "error", "message": "unknown command"])
        done()
    }
}

func pump() {
    guard !busy else { return }
    if closing {
        center.removeAllDeliveredNotifications()
        center.removeAllPendingNotificationRequests()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { exit(0) }
        return
    }
    guard !queue.isEmpty else { return }
    busy = true
    started += 1
    let current = started
    DispatchQueue.main.asyncAfter(deadline: .now() + 10) {
        guard busy, started == current else { return }
        emit(["event": "error", "message": "a notification command stalled for 10s; restarting the notifier"])
        exit(1)
    }
    handle(queue.removeFirst()) {
        busy = false
        pump()
    }
}

func enqueue(_ command: [String: Any]) {
    guard !closing else { return }
    queue.append(command)
    pump()
}

func shutdown() {
    guard !closing else { return }
    closing = true
    queue.removeAll()
    DispatchQueue.main.asyncAfter(deadline: .now() + 3) { exit(0) }
    pump()
}

final class Delegate: NSObject, UNUserNotificationCenterDelegate {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list, .sound])
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        let action: String
        switch response.actionIdentifier {
        case UNNotificationDefaultActionIdentifier: action = "opened"
        case UNNotificationDismissActionIdentifier: action = "closed"
        default: action = "snoozed"
        }
        emit([
            "event": "response",
            "key": info["key"] as? String ?? "",
            "generation": info["generation"] as? Int ?? 0,
            "action": action,
            "value": response.actionIdentifier,
        ])
        completionHandler()
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let delegate = Delegate()
center.delegate = delegate
var signalSources: [DispatchSourceSignal] = []

if flag("clear") {
    shutdown()
} else if flag("request-permission") {
    center.requestAuthorization(options: [.alert, .sound]) { _, _ in exit(0) }
    DispatchQueue.main.asyncAfter(deadline: .now() + 60) { exit(0) }
} else {
    let actions = (option("actions") ?? "").split(separator: ",").map(String.init).filter { !$0.isEmpty }
    let mute = option("mute-action").map { [UNNotificationAction(identifier: "mute-today", title: $0, options: [])] } ?? []
    center.setNotificationCategories([
        UNNotificationCategory(
            identifier: "snoozable",
            actions: actions.map { UNNotificationAction(identifier: $0, title: "Snooze \($0)", options: []) } + mute,
            intentIdentifiers: [],
            options: [.customDismissAction]
        ),
        UNNotificationCategory(identifier: "oneshot", actions: mute, intentIdentifiers: [], options: [.customDismissAction]),
    ])
    for sig in [SIGTERM, SIGINT, SIGHUP] {
        signal(sig, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
        source.setEventHandler(handler: shutdown)
        source.resume()
        signalSources.append(source)
    }
    center.getNotificationSettings { settings in
        if settings.authorizationStatus == .notDetermined {
            center.requestAuthorization(options: [.alert, .sound]) { _, _ in refreshAuthorization() }
        }
        DispatchQueue.main.async { report(settings.authorizationStatus) }
    }
    Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { _ in refreshAuthorization() }
    Thread.detachNewThread {
        while let line = readLine() {
            guard let data = line.data(using: .utf8),
                  let command = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            else { continue }
            DispatchQueue.main.async { enqueue(command) }
        }
        DispatchQueue.main.async(execute: shutdown)
    }
}
app.run()
