#!/usr/bin/env swift

import CoreFoundation
import Foundation
import Security

enum SearchListError: Error, CustomStringConvertible {
    case osStatus(String, OSStatus)
    case invalidArguments
    case pathTooLong
    case targetStillPresent(String)
    case testAssertion(String)

    var description: String {
        switch self {
        case let .osStatus(operation, status):
            let detail = SecCopyErrorMessageString(status, nil) as String? ?? "unknown error"
            return "\(operation) failed (\(status)): \(detail)"
        case .invalidArguments:
            return "usage: configure-macos-signing-keychain.swift add|remove|list-json|verify-absent|self-test [KEYCHAIN ...]"
        case .pathTooLong:
            return "a keychain path exceeded the Security framework's buffer"
        case let .targetStillPresent(path):
            return "the signing keychain remains on the user search list: \(path)"
        case let .testAssertion(message):
            return "native search-list regression failed: \(message)"
        }
    }
}

func checked(_ operation: String, _ body: () -> OSStatus) throws {
    let status = body()
    guard status == errSecSuccess else {
        throw SearchListError.osStatus(operation, status)
    }
}

func searchList() throws -> [SecKeychain] {
    var value: CFArray?
    try checked("SecKeychainCopyDomainSearchList") {
        SecKeychainCopyDomainSearchList(.user, &value)
    }
    return (value as? [SecKeychain]) ?? []
}

func keychainPath(_ keychain: SecKeychain) throws -> String {
    var capacity: UInt32 = UInt32(PATH_MAX)
    var bytes = [CChar](repeating: 0, count: Int(capacity) + 1)
    try checked("SecKeychainGetPath") {
        SecKeychainGetPath(keychain, &capacity, &bytes)
    }
    guard Int(capacity) < bytes.count else {
        throw SearchListError.pathTooLong
    }
    bytes[Int(capacity)] = 0
    return FileManager.default.string(withFileSystemRepresentation: bytes, length: Int(capacity))
}

func normalized(_ path: String) -> String {
    URL(fileURLWithPath: NSString(string: path).expandingTildeInPath)
        .standardizedFileURL.resolvingSymlinksInPath().path
}

func setSearchList(
    _ keychains: [SecKeychain],
    using setter: (CFArray) -> OSStatus
) throws {
    try checked("SecKeychainSetDomainSearchList") { setter(keychains as CFArray) }
}

func setUserSearchList(_ keychains: [SecKeychain]) throws {
    try setSearchList(keychains) {
        SecKeychainSetDomainSearchList(.user, $0)
    }
}

func openedKeychain(at path: String) throws -> SecKeychain {
    var opened: SecKeychain?
    try checked("SecKeychainOpen") {
        SecKeychainOpen(path, &opened)
    }
    guard let opened else {
        throw SearchListError.osStatus("SecKeychainOpen", errSecInvalidKeychain)
    }
    return opened
}

func transformedSearchList(
    action: String,
    targetPath: String,
    existing: [SecKeychain],
    openedTarget: SecKeychain? = nil
) throws -> [SecKeychain] {
    let target = normalized(targetPath)
    var retained: [SecKeychain] = []
    for keychain in existing where normalized(try keychainPath(keychain)) != target {
        retained.append(keychain)
    }

    if action == "add" {
        let opened = try openedTarget ?? openedKeychain(at: targetPath)
        retained.insert(opened, at: 0)
    }
    return retained
}

func applySearchListUpdate(
    action: String,
    targetPath: String,
    read: () throws -> [SecKeychain],
    open: (String) throws -> SecKeychain,
    write: ([SecKeychain]) throws -> Void
) throws {
    let existing = try read()
    let updated = try transformedSearchList(
        action: action,
        targetPath: targetPath,
        existing: existing,
        openedTarget: action == "add" ? open(targetPath) : nil
    )
    try write(updated)
}

func updateSearchList(action: String, targetPath: String) throws {
    try applySearchListUpdate(
        action: action,
        targetPath: targetPath,
        read: searchList,
        open: openedKeychain,
        write: setUserSearchList
    )
}

func listPaths() throws -> [String] {
    try searchList().map(keychainPath)
}

func assertPaths(_ actual: [String], _ expected: [String], _ message: String) throws {
    guard actual.map(normalized) == expected.map(normalized) else {
        throw SearchListError.testAssertion(message)
    }
}

func assertKeychains(
    _ actual: [SecKeychain],
    _ expectedPaths: [String],
    _ message: String
) throws {
    try assertPaths(actual.map(keychainPath), expectedPaths, message)
}

func createTestKeychain(at path: String) throws -> SecKeychain {
    let password = Array("kettle-disposable-test-keychain".utf8)
    var created: SecKeychain?
    let status = password.withUnsafeBytes { passwordBytes in
        SecKeychainCreate(
            path,
            UInt32(passwordBytes.count),
            passwordBytes.baseAddress,
            false,
            nil,
            &created
        )
    }
    try checked("SecKeychainCreate") { status }
    guard let created else {
        throw SearchListError.osStatus("SecKeychainCreate", errSecInvalidKeychain)
    }
    return created
}

/// Exercise the production transformation and Security.framework path bridge
/// without ever changing the developer's real search list. A test process can
/// be killed without Swift running `defer`, so even a temporary user-domain
/// mutation here would be unsafe on a workstation and on a cancelled CI job.
func runNativeRegression(in directory: String) throws {
    let original = try searchList()
    let originalPaths = try original.map(keychainPath)
    let existingPath = URL(fileURLWithPath: directory)
        .appendingPathComponent("existing \"quoted\" \\ keychain.keychain-db").path
    let ephemeralPath = URL(fileURLWithPath: directory)
        .appendingPathComponent("kettle-signing.keychain-db").path
    let existing = try createTestKeychain(at: existingPath)
    var disposable = [existing]
    defer {
        for keychain in disposable.reversed() {
            let status = SecKeychainDelete(keychain)
            if status != errSecSuccess && status != errSecInvalidKeychain {
                FileHandle.standardError.write(
                    Data("macOS signing keychain test cleanup failed (\(status))\n".utf8)
                )
                exit(1)
            }
        }
    }
    let ephemeral = try createTestKeychain(at: ephemeralPath)
    disposable.append(ephemeral)
    try assertKeychains([existing], [existingPath], "an odd keychain path was not preserved")
    try assertPaths(
        try listPaths(),
        originalPaths,
        "SecKeychainCreate unexpectedly mutated the user search list"
    )

    let withExisting = [existing] + original
    try assertKeychains(
        withExisting,
        [existingPath] + originalPaths,
        "the odd-path fixture could not be represented losslessly"
    )
    var written: [SecKeychain]?
    try applySearchListUpdate(
        action: "add",
        targetPath: ephemeralPath,
        read: { withExisting },
        open: { _ in ephemeral },
        write: { written = $0 }
    )
    let added = try written ?? {
        throw SearchListError.testAssertion("the add path never called its writer")
    }()
    try applySearchListUpdate(
        action: "add",
        targetPath: ephemeralPath,
        read: { added },
        open: { _ in ephemeral },
        write: { written = $0 }
    )
    let addedAgain = try written ?? {
        throw SearchListError.testAssertion("the repeated add never called its writer")
    }()
    try assertKeychains(
        addedAgain,
        [ephemeralPath, existingPath] + originalPaths,
        "adding the signing keychain twice did not de-duplicate it"
    )
    try applySearchListUpdate(
        action: "remove",
        targetPath: ephemeralPath,
        read: { addedAgain },
        open: { _ in
            throw SearchListError.testAssertion("remove tried to open the target")
        },
        write: { written = $0 }
    )
    let removed = try written ?? {
        throw SearchListError.testAssertion("the remove path never called its writer")
    }()
    try assertKeychains(
        removed,
        [existingPath] + originalPaths,
        "removing the signing keychain disturbed an existing entry"
    )
    try applySearchListUpdate(
        action: "remove",
        targetPath: existingPath,
        read: { removed },
        open: { _ in
            throw SearchListError.testAssertion("remove tried to open the target")
        },
        write: { written = $0 }
    )
    let originalAgain = try written ?? {
        throw SearchListError.testAssertion("the final remove never called its writer")
    }()
    try assertKeychains(originalAgain, originalPaths, "removal did not recover the original list")

    // The old cleanup rejected an empty original list before calling the API.
    // Drive the exact composition with injected operations and pin that its
    // writer receives an empty list, without touching the user domain.
    written = nil
    try applySearchListUpdate(
        action: "remove",
        targetPath: ephemeralPath,
        read: { [] },
        open: { _ in
            throw SearchListError.testAssertion("empty remove tried to open the target")
        },
        write: { written = $0 }
    )
    guard written?.isEmpty == true else {
        throw SearchListError.testAssertion("an empty list did not reach the writer")
    }
    try assertPaths(try listPaths(), originalPaths, "the self-test mutated the user search list")
}

func main() throws {
    let arguments = Array(CommandLine.arguments.dropFirst())
    guard let action = arguments.first else {
        throw SearchListError.invalidArguments
    }
    switch action {
    case "add", "remove":
        guard arguments.count == 2 else {
            throw SearchListError.invalidArguments
        }
        try updateSearchList(action: action, targetPath: arguments[1])
    case "list-json":
        guard arguments.count == 1 else {
            throw SearchListError.invalidArguments
        }
        let data = try JSONSerialization.data(withJSONObject: listPaths())
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0a]))
    case "verify-absent":
        guard arguments.count == 2 else {
            throw SearchListError.invalidArguments
        }
        let target = normalized(arguments[1])
        if try listPaths().contains(where: { normalized($0) == target }) {
            throw SearchListError.targetStillPresent(arguments[1])
        }
    case "self-test":
        guard arguments.count == 2 else {
            throw SearchListError.invalidArguments
        }
        try runNativeRegression(in: arguments[1])
    default:
        throw SearchListError.invalidArguments
    }
}

do {
    try main()
} catch {
    FileHandle.standardError.write(Data("macOS signing keychain search-list update failed: \(error)\n".utf8))
    exit(1)
}
