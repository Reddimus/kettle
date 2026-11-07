#!/usr/bin/env python3
"""Hermetic tests for release package-template rendering."""

from __future__ import annotations

import hashlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("render-package-templates.py")
ROOT = SCRIPT.parent.parent


class PackageTemplateTests(unittest.TestCase):
    def test_linux_archive_notice_carries_license_for_existing_updaters(self) -> None:
        workflow = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        package = workflow.split("- name: Package (Linux)", 1)[1]
        docs = package.split("cp LICENSE NOTICE README.md CHANGELOG.md dist/kettle/", 1)[1]
        docs = docs.split("# Desktop integration assets", 1)[0]
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "dist/kettle/docs").mkdir(parents=True)
            shutil.copy2(ROOT / "NOTICE", root / "dist/kettle/NOTICE")
            # Use the actual document sources without copying the repository.
            shutil.copytree(ROOT / "docs/changelog", root / "docs/changelog")
            shutil.copytree(ROOT / "docs/licenses", root / "docs/licenses")
            result = subprocess.run(
                ["bash", "-e", "-c", docs], cwd=root,
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            expected = (ROOT / "docs/licenses/tokyonight.nvim/LICENSE").read_bytes()
            self.assertTrue((root / "dist/kettle/NOTICE").read_bytes().endswith(expected))
            for name in ("LICENSE", "README.md"):
                relative = Path("docs/licenses/tokyonight.nvim") / name
                self.assertEqual((root / "dist/kettle" / relative).read_bytes(),
                                 (ROOT / relative).read_bytes())

    def test_nix_installs_tokyonight_license_bytes_on_both_platforms(self) -> None:
        install = shutil.which("ginstall") or shutil.which("install")
        if not install or sys.platform == "win32" or (sys.platform == "darwin" and not shutil.which("ginstall")):
            self.skipTest("GNU install is required to execute the Nix install hook")
        flake = (ROOT / "flake.nix").read_text(encoding="utf-8")
        hook = flake.split("postInstall = ", 1)[1]
        # Only the unconditional part runs on Darwin as well as Linux.
        common = hook.split("''", 2)[1] if hook.startswith("''") else ""
        with tempfile.TemporaryDirectory() as raw:
            prefix = Path(raw) / "out"
            result = subprocess.run(
                ["bash", "-e", "-c", 'out=$1; install_tool=$2\n'
                 'install() { command "$install_tool" "$@"; }\n' + common,
                 "bash", str(prefix), install],
                cwd=ROOT, capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            for name in ("LICENSE", "README.md"):
                relative = Path("docs/licenses/tokyonight.nvim") / name
                installed = prefix / "share/doc/kettle" / relative
                self.assertEqual(installed.read_bytes(), (ROOT / relative).read_bytes())
                self.assertEqual(installed.stat().st_mode & 0o777, 0o644)

    def test_update_manual_describes_supported_layouts(self) -> None:
        manual = (ROOT / "packaging/linux/kettle.1").read_text(encoding="utf-8")
        update = manual.rsplit(".B kettle update", 1)[1].split(".SH KEY BINDINGS", 1)[0]
        update = " ".join(update.split())
        self.assertIn("official installer-owned Linux x86_64/aarch64 layouts", update)
        self.assertIn("eligible official macOS kettle.app bundles", update)
        self.assertIn("Homebrew-managed copies must update through Homebrew", update)

    def seed_package(self, root: Path) -> Path:
        source = root / "source" / "kettle"
        source.mkdir(parents=True)
        for name in ("kettle", "kettle-media-worker", "LICENSE", "NOTICE", "README.md", "CHANGELOG.md"):
            (source / name).write_text(name, encoding="utf-8")
        (source / "packaging/linux").mkdir(parents=True)
        for name in ("kettle.desktop", "kettle.svg", "kettle-256.png", "kettle.1"):
            (source / "packaging/linux" / name).write_text(name, encoding="utf-8")
        (source / "docs/changelog").mkdir(parents=True)
        (source / "docs/changelog/1.0.md").write_text("history", encoding="utf-8")
        shutil.copytree(ROOT / "docs/licenses", source / "docs/licenses")
        return source

    def assert_installed_licenses(self, source: Path, prefix: Path) -> None:
        for original in sorted((source / "docs/licenses").rglob("*")):
            if original.is_file():
                relative = original.relative_to(source)
                installed = prefix / "share/doc/kettle" / relative
                self.assertTrue(installed.is_file(), f"missing installed {relative}")
                self.assertEqual(installed.read_bytes(), original.read_bytes(), str(relative))

    def test_arch_preserves_renderer_licenses(self) -> None:
        install = shutil.which("ginstall") or shutil.which("install")
        if not install or sys.platform == "win32" or (sys.platform == "darwin" and not shutil.which("ginstall")):
            self.skipTest("GNU install is required to execute the Arch package function")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = self.seed_package(root)
            result = subprocess.run(
                ["bash", "-e", "-c", 'srcdir=$2; pkgdir=$3; install_tool=$4\n'
                 'install() { command "$install_tool" "$@"; }\nsource "$1"\npackage',
                 "bash", str(ROOT / "packaging/arch/PKGBUILD.in"),
                 str(source.parent), str(root / "package"), install],
                check=False, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assert_installed_licenses(source, root / "package/usr")

    @unittest.skipUnless(shutil.which("ruby"), "Ruby is required to execute the Homebrew install method")
    def test_homebrew_preserves_renderer_licenses(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = self.seed_package(root)
            result = subprocess.run(
                ["ruby", "-e", '''
require "pathname"
require "fileutils"
module OS
  def self.mac?
    false
  end
end
class Pathname
  def install(*sources)
    mkpath
    sources.each do |source|
      if source.is_a?(Hash)
        source.each { |from, to| FileUtils.cp_r(from, self/to) }
      else
        FileUtils.cp_r(source, self)
      end
    end
  end
end
class Formula
  def self.method_missing(*)
    nil
  end
  def self.test
  end
  def prefix
    Pathname.new(ARGV[1])
  end
  def bin
    prefix/"bin"
  end
  def man1
    prefix/"share/man/man1"
  end
  def resource(*)
    self
  end
  def stage
    yield
  end
end
load ARGV[0]
Kettle.new.install
''', str(ROOT / "packaging/homebrew/kettle.rb.in"), str(root / "prefix")],
                cwd=source, check=False, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assert_installed_licenses(source, root / "prefix")

    def run_renderer(
        self, root: Path, version: str = "9.8.7"
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--version",
                version,
                "--release-manifest",
                str(root / "manifest.json"),
                "--macos-archive",
                str(root / "mac.zip"),
                "--linux-aarch64-archive",
                str(root / "linux-aarch64.tar.gz"),
                "--linux-x86-64-archive",
                str(root / "linux.tar.gz"),
                "--output-dir",
                str(root / "out"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_renders_exact_archive_hashes_deterministically(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = b'{"schema_version":1}\n'
            macos = b"macos release archive\n"
            linux_aarch64 = b"linux aarch64 release archive\n"
            linux = b"linux release archive\n"
            (root / "manifest.json").write_bytes(manifest)
            (root / "mac.zip").write_bytes(macos)
            (root / "linux-aarch64.tar.gz").write_bytes(linux_aarch64)
            (root / "linux.tar.gz").write_bytes(linux)

            first = self.run_renderer(root)
            self.assertEqual(first.returncode, 0, first.stderr)
            formula = (root / "out" / "kettle.rb").read_text(encoding="utf-8")
            pkgbuild = (root / "out" / "PKGBUILD").read_text(encoding="utf-8")
            first_bytes = (formula.encode(), pkgbuild.encode())

            self.assertIn(
                '  url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-update-manifest.json",\n'
                "      using: :nounzip\n"
                f'  sha256 "{hashlib.sha256(manifest).hexdigest()}"',
                formula,
            )
            self.assertIn(
                '      url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-macos-universal.zip"\n'
                f'      sha256 "{hashlib.sha256(macos).hexdigest()}"',
                formula,
            )
            self.assertIn(
                '        url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-linux-aarch64.tar.gz"\n'
                f'        sha256 "{hashlib.sha256(linux_aarch64).hexdigest()}"',
                formula,
            )
            self.assertIn(
                '        url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-linux-x86_64.tar.gz"\n'
                f'        sha256 "{hashlib.sha256(linux).hexdigest()}"',
                formula,
            )
            self.assertIn("pkgver=9.8.7", pkgbuild)
            depends = pkgbuild.split("depends=(", 1)[1].split(")", 1)[0]
            self.assertIn("'wayland'", depends, "Arch provides libwayland-client in wayland")
            self.assertNotIn("'libwayland-client'", depends)
            self.assertIn(hashlib.sha256(linux).hexdigest(), pkgbuild)
            self.assertNotIn("@VERSION@", formula + pkgbuild)
            self.assertIn(
                '    share_dir = prefix/"share"\n'
                '    resource("binary").stage do\n'
                "      if OS.mac?",
                formula,
            )
            self.assertNotIn('\n      share = prefix/"share"\n', formula)
            self.assertIn(
                '(share_dir/"doc/kettle").install "#{doc_dir}/#{f}"',
                formula,
            )
            self.assertIn(
                '(share_dir/"doc/kettle").install "#{doc_dir}/shell-integration"',
                formula,
            )
            self.assertIn(
                '(share_dir/"doc/kettle/docs").install "#{doc_dir}/docs/changelog"',
                formula,
            )
            self.assertIn(
                'install -m644 docs/changelog/*.md',
                pkgbuild,
            )
            self.assertIn('bin.install "kettle", "kettle-media-worker"', formula)
            self.assertIn('install -Dm755 kettle-media-worker "${pkgdir}/usr/bin/kettle-media-worker"', pkgbuild)

            second = self.run_renderer(root)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first_bytes[0], (root / "out" / "kettle.rb").read_bytes())
            self.assertEqual(first_bytes[1], (root / "out" / "PKGBUILD").read_bytes())

    def test_rejects_non_stable_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "manifest.json").write_bytes(b"{}\n")
            (root / "mac.zip").write_bytes(b"mac")
            (root / "linux-aarch64.tar.gz").write_bytes(b"linux-aarch64")
            (root / "linux.tar.gz").write_bytes(b"linux")
            result = self.run_renderer(root, "9.8.7-rc.1")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid stable version", result.stderr)


if __name__ == "__main__":
    unittest.main()
