import AppKit
import Foundation

let args = CommandLine.arguments
guard args.count == 3, let pid = Int(args[1]), pid > 0,
      let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)
        as? [[String: Any]] else {
    fputs("invalid native window query\n", stderr)
    exit(1)
}
let matches = rows.filter {
    ($0[kCGWindowOwnerPID as String] as? Int) == pid
        && ($0[kCGWindowName as String] as? String) == args[2]
        && ($0[kCGWindowLayer as String] as? Int) == 0
}
guard matches.count == 1 else {
    fputs("expected one owned native window\n", stderr)
    exit(1)
}
let data = try JSONSerialization.data(withJSONObject: matches[0])
print(String(decoding: data, as: UTF8.self))
