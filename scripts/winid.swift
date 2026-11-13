// Print the on-screen normal windows owned by a given PID, one per line, as
//   <windowNumber> <x> <y> <width> <height>
// in front-to-back order (frontmost first). Used by scripts/screenshots.sh to feed
// `screencapture -l<id>` (window capture) and `screencapture -R x,y,w,h` (region capture).
// Run with: swift scripts/winid.swift <pid>
import CoreGraphics
import Foundation

guard CommandLine.arguments.count > 1, let pid = Int(CommandLine.arguments[1]) else {
    FileHandle.standardError.write(Data("usage: winid <pid>\n".utf8))
    exit(2)
}

let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    exit(1)
}

for info in list {
    guard let owner = info[kCGWindowOwnerPID as String] as? Int, owner == pid else { continue }
    // Layer 0 = normal app windows (skip menubar/status/overlay layers).
    let layer = info[kCGWindowLayer as String] as? Int ?? 0
    if layer != 0 { continue }
    guard let num = info[kCGWindowNumber as String] as? Int,
          let b = info[kCGWindowBounds as String] as? [String: Any],
          let x = b["X"] as? Double, let y = b["Y"] as? Double,
          let w = b["Width"] as? Double, let h = b["Height"] as? Double
    else { continue }
    print("\(num) \(Int(x)) \(Int(y)) \(Int(w)) \(Int(h))")
}
