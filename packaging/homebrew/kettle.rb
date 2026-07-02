# Homebrew formula for kettle
#
# To set up a tap so macOS / Linux Homebrew users can install kettle:
#
#   1. Create a public GitHub repo named `homebrew-kettle` under the
#      same org/user as kettle (Homebrew expects the `homebrew-` prefix
#      so `brew tap reddimus/kettle` resolves to `Reddimus/homebrew-
#      kettle`).
#   2. Copy this file into that repo as `Formula/kettle.rb`.
#   3. End users then run:
#         brew tap reddimus/kettle
#         brew install kettle
#
# Per-release maintenance:
#
#   On every new tag, bump the `version` line below + the two
#   `sha256` lines. The hashes live next to the artifacts on the
#   release page (the cycle-254 `.sha256` sidecars). Fetch them via:
#
#     curl -fsSL https://github.com/Reddimus/kettle/releases/download/v<VER>/kettle-macos-universal.zip.sha256
#     curl -fsSL https://github.com/Reddimus/kettle/releases/download/v<VER>/kettle-linux-x86_64.tar.gz.sha256
#
#   Each prints `<sha>  <filename>` — drop the hex into the matching
#   field. The `head` of `livecheck` below auto-detects the latest
#   tag so `brew livecheck kettle` flags drift.
#
# This file is a template — it does NOT live in a tap repo yet.
# See `packaging/homebrew/README.md` for the full setup walkthrough.

class Kettle < Formula
  desc "Fast, cross-platform, GPU-accelerated terminal emulator written in Rust"
  homepage "https://github.com/Reddimus/kettle"
  license "MIT"
  version "2.33.1"

  on_macos do
    # macOS ships the universal2 .app bundle — same binary covers
    # arm64 and x86_64. No need to split by architecture.
    url "https://github.com/Reddimus/kettle/releases/download/v#{version}/kettle-macos-universal.zip"
    sha256 "c0cb367cf231f1e0e66dd201b8135ab19ad97873c6743897f521c88914e84443"
  end

  on_linux do
    url "https://github.com/Reddimus/kettle/releases/download/v#{version}/kettle-linux-x86_64.tar.gz"
    sha256 "61d572cb90cdf12d4321ce7c27588f6a95fcba1029b0474bed8cb821a431e711"
  end

  livecheck do
    # `latest_release` follows the same `/releases/latest` redirect
    # the cycle-253 `install-online.sh` uses to resolve the current
    # version — kettle ships one binary per tag, no point releases
    # of older majors, so this is the right canonical signal.
    url :stable
    strategy :github_latest
  end

  def install
    if OS.mac?
      # The macOS zip contains `kettle.app` at the root.
      prefix.install "kettle.app"
      # Symlink the binary inside the .app into bin/ so `kettle` on
      # the user's PATH works after `brew install`. `write_exec_script`
      # would also work but `bin.install_symlink` is the canonical
      # pattern for .app-wrapped CLIs on Homebrew (matches the
      # `macvim`, `ghostty`, etc. formulae).
      bin.install_symlink prefix/"kettle.app/Contents/MacOS/kettle"
    else
      # The Linux tarball extracts to a `kettle/` directory with
      # the binary + LICENSE + NOTICE + README + CHANGELOG +
      # packaging assets at the root.
      bin.install "kettle/kettle"
      # Cycle 279: man page so `man kettle` works after install.
      if File.exist?("kettle/packaging/linux/kettle.1")
        man1.install "kettle/packaging/linux/kettle.1"
      end
      # Keep the XDG launcher + icons so a `brew install`-ed kettle
      # also shows up in GNOME Activities / Ubuntu Super-key search
      # on Homebrew-on-Linux users (Linuxbrew). Same paths the
      # cycle-0 `install.sh` writes to.
      share = prefix/"share"
      (share/"applications").install "kettle/packaging/linux/kettle.desktop"
      (share/"icons/hicolor/scalable/apps").install "kettle/packaging/linux/kettle.svg"
      Dir.glob("kettle/packaging/linux/kettle-*.png").each do |png|
        # png filename is e.g. `kettle-256.png` — extract the size.
        size = png[/kettle-(\d+)\.png/, 1]
        (share/"icons/hicolor/#{size}x#{size}/apps").install(png => "kettle.png")
      end
    end
    # Per-platform docs that ship in the release archives (LICENSE,
    # NOTICE, README, CHANGELOG, shell-integration/) — drop them
    # under share/doc so users have an offline reference.
    doc_dir = OS.mac? ? "kettle.app/Contents/Resources" : "kettle"
    %w[LICENSE NOTICE README.md CHANGELOG.md].each do |f|
      next unless File.exist?("#{doc_dir}/#{f}")

      (share/"doc/kettle").install "#{doc_dir}/#{f}"
    end
  end

  test do
    # Smoke test: the binary boots, prints its version, and exits
    # cleanly. The version string follows cycle-192's format —
    # `kettle X.Y.Z (sha12)` on a git checkout, `kettle X.Y.Z` on a
    # vendored / tarball build. The version may differ at build time
    # if homebrew bumps the formula independently of the upstream
    # release — assert only the leading `kettle ` prefix.
    assert_match(/^kettle [0-9]+\.[0-9]+\.[0-9]+/, shell_output("#{bin}/kettle --version"))
  end
end
