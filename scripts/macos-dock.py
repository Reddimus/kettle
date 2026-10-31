"""Read and operate a candidate's retained native Dock tile, without index lookup."""

import ctypes
import contextlib
import os
import json
from pathlib import Path
import subprocess
import time


def same_bundle(left, right):
    if left is None:
        return False
    try:
        return left.samefile(right)
    except FileNotFoundError:
        return False


def candidate_pids(bundle):
    bundle.stat()
    script = """ObjC.import('AppKit');
var apps = $.NSWorkspace.sharedWorkspace.runningApplications;
var rows = [];
for (var i = 0; i < apps.count; i++) {
 var app = apps.objectAtIndex(i);
 if (app.bundleURL) rows.push([app.processIdentifier, ObjC.unwrap(app.bundleURL.path)]);
}
JSON.stringify(rows);"""
    result = subprocess.run(['launchctl', 'asuser', str(os.getuid()), 'osascript',
                             '-l', 'JavaScript', '-e', script],
                            check=True, capture_output=True, text=True, timeout=15)
    return sorted(pid for pid, path in json.loads(result.stdout)
                  if same_bundle(Path(path), bundle))


class Dock:
    def __init__(self):
        self.cf = ctypes.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')
        self.ax = ctypes.CDLL('/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices')
        self.owned = []
        p = ctypes.c_void_p
        signatures = [
            (self.cf, 'CFStringCreateWithCString', [p, ctypes.c_char_p, ctypes.c_uint32], p),
            (self.cf, 'CFStringGetCString', [p, ctypes.c_char_p, ctypes.c_long, ctypes.c_uint32], ctypes.c_bool),
            (self.cf, 'CFArrayGetCount', [p], ctypes.c_long),
            (self.cf, 'CFArrayGetValueAtIndex', [p, ctypes.c_long], p),
            (self.cf, 'CFURLGetFileSystemRepresentation', [p, ctypes.c_bool, ctypes.c_char_p, ctypes.c_long], ctypes.c_bool),
            (self.cf, 'CFBooleanGetValue', [p], ctypes.c_bool),
            (self.cf, 'CFGetTypeID', [p], ctypes.c_ulong),
            (self.cf, 'CFURLGetTypeID', [], ctypes.c_ulong),
            (self.cf, 'CFStringGetTypeID', [], ctypes.c_ulong),
            (self.cf, 'CFArrayGetTypeID', [], ctypes.c_ulong),
            (self.cf, 'CFBooleanGetTypeID', [], ctypes.c_ulong),
            (self.cf, 'CFRelease', [p], None),
            (self.ax, 'AXUIElementCreateApplication', [ctypes.c_int], p),
            (self.ax, 'AXUIElementCopyAttributeValue', [p, p, ctypes.POINTER(p)], ctypes.c_int),
            (self.ax, 'AXUIElementPerformAction', [p, p], ctypes.c_int),
        ]
        for library, name, args, result in signatures:
            function = getattr(library, name)
            function.argtypes, function.restype = args, result

    def close(self):
        for value in reversed(self.owned):
            self.cf.CFRelease(value)
        self.owned.clear()

    def string(self, text):
        value = self.cf.CFStringCreateWithCString(None, text.encode(), 0x08000100)
        if not value:
            raise RuntimeError('cannot allocate native string')
        self.owned.append(value)
        return value

    def attr(self, element, name):
        value = ctypes.c_void_p()
        status = self.ax.AXUIElementCopyAttributeValue(element, self.string(name), ctypes.byref(value))
        if status in (-25212, -25205, -25204, -25202):
            return None
        if status != 0:
            raise RuntimeError(f'native Dock attribute {name} failed: {status}')
        if value.value:
            self.owned.append(value.value)
        return value.value

    def typed(self, value, kind):
        return value and self.cf.CFGetTypeID(value) == getattr(self.cf, f'CF{kind}GetTypeID')()

    def text(self, value):
        buffer = ctypes.create_string_buffer(8192)
        if self.typed(value, 'String') and self.cf.CFStringGetCString(value, buffer, len(buffer), 0x08000100):
            return buffer.value.decode()
        return None

    def path(self, value):
        buffer = ctypes.create_string_buffer(8192)
        if self.typed(value, 'URL') and self.cf.CFURLGetFileSystemRepresentation(value, True, buffer, len(buffer)):
            return Path(buffer.value.decode()).resolve()
        return None

    def children(self, element):
        value = self.attr(element, 'AXChildren')
        if not self.typed(value, 'Array'):
            return []
        count = self.cf.CFArrayGetCount(value)
        if count > 256:
            raise RuntimeError('native Dock child limit exceeded')
        return [self.cf.CFArrayGetValueAtIndex(value, i) for i in range(count)]

    def action(self, element, name):
        status = self.ax.AXUIElementPerformAction(element, self.string(name))
        if status:
            raise RuntimeError(f'native Dock {name} failed: {status}')

    def command(self, bundle, operation, validate):
        dock_pid = int(subprocess.check_output(
            ['pgrep', '-x', '-u', str(os.getuid()), 'Dock'], text=True, timeout=10).strip())
        root = self.ax.AXUIElementCreateApplication(dock_pid)
        self.owned.append(root)
        tiles = []
        for child in self.children(root):
            for tile in self.children(child):
                running = self.attr(tile, 'AXIsApplicationRunning')
                if (same_bundle(self.path(self.attr(tile, 'AXURL')), bundle)
                        and self.typed(running, 'Boolean') and self.cf.CFBooleanGetValue(running)):
                    tiles.append(tile)
        if not tiles:
            return '' if operation == 'rows' else 'not-found'
        if len(tiles) != 1:
            raise RuntimeError('candidate Dock tile is ambiguous')
        validate()
        clicked = False
        menus = []
        try:
            self.action(tiles[0], 'AXShowMenu')
            menus = self.children(tiles[0])
            time.sleep(1.5)
            validate()
            rows = []
            menus = self.children(tiles[0])
            for menu in menus:
                if self.text(self.attr(menu, 'AXRole')) != 'AXMenu':
                    continue
                for item in self.children(menu):
                    title = self.text(self.attr(item, 'AXTitle'))
                    if title:
                        rows.append((title, item))
            if operation == 'rows':
                return '\n'.join(title for title, _ in rows)
            matches = [item for title, item in rows if title == operation]
            if not matches:
                return 'not-found'
            if len(matches) != 1:
                raise RuntimeError(f'candidate Dock menu has ambiguous {operation}')
            validate()
            self.action(matches[0], 'AXPress')
            clicked = True
            return 'clicked'
        finally:
            if not clicked:
                with contextlib.suppress(RuntimeError):
                    for menu in menus or self.children(tiles[0]):
                        with contextlib.suppress(RuntimeError):
                            self.action(menu, 'AXCancel')
