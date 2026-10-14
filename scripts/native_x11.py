"""Native input for isolated X11 test fixtures. Never use on the owner's desktop."""

import ctypes
import re
import subprocess


class X11Input:
    def __init__(self):
        self.x = ctypes.CDLL("libX11.so.6")
        self.xt = ctypes.CDLL("libXtst.so.6")
        pointer, window = ctypes.c_void_p, ctypes.c_ulong
        declarations = (
            (self.x.XOpenDisplay, [ctypes.c_char_p], pointer),
            (self.x.XCloseDisplay, [pointer], ctypes.c_int),
            (self.x.XSync, [pointer, ctypes.c_int], ctypes.c_int),
            (self.x.XSetInputFocus, [pointer, window, ctypes.c_int, window], ctypes.c_int),
            (self.x.XStringToKeysym, [ctypes.c_char_p], window),
            (self.x.XKeysymToKeycode, [pointer, window], ctypes.c_ubyte),
            (self.x.XWarpPointer, [pointer, window, window, ctypes.c_int, ctypes.c_int,
                                  ctypes.c_uint, ctypes.c_uint, ctypes.c_int, ctypes.c_int], ctypes.c_int),
            (self.xt.XTestFakeKeyEvent, [pointer, ctypes.c_uint, ctypes.c_int, window], ctypes.c_int),
            (self.xt.XTestFakeButtonEvent, [pointer, ctypes.c_uint, ctypes.c_int, window], ctypes.c_int),
        )
        for function, arguments, result in declarations:
            function.argtypes, function.restype = arguments, result
        self.display = self.x.XOpenDisplay(None)
        if not self.display:
            raise RuntimeError("native Linux input requires an isolated X11 display")

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.x.XCloseDisplay(self.display)

    @staticmethod
    def windows(pid):
        tree = subprocess.check_output(["xwininfo", "-root", "-tree"], text=True, timeout=3)
        owned = []
        for window in re.findall(r"^\s+(0x[0-9a-fA-F]+)", tree, re.MULTILINE):
            prop = subprocess.run(["xprop", "-id", window, "_NET_WM_PID"],
                                  capture_output=True, text=True, timeout=3)
            if re.search(rf"= {pid}\s*$", prop.stdout):
                owned.append(int(window, 16))
        return owned

    def sync(self):
        self.x.XSync(self.display, 0)

    def focus(self, window):
        self.x.XSetInputFocus(self.display, window, 1, 0)
        self.sync()

    def keys(self, *names):
        codes = [self.x.XKeysymToKeycode(self.display, self.x.XStringToKeysym(name.encode()))
                 for name in names]
        if not all(codes):
            raise RuntimeError("native key has no X11 keycode")
        pressed = []
        try:
            for code in codes:
                if not self.xt.XTestFakeKeyEvent(self.display, code, 1, 0):
                    raise RuntimeError("X11 refused native key input")
                pressed.append(code)
        finally:
            for code in reversed(pressed):
                self.xt.XTestFakeKeyEvent(self.display, code, 0, 0)
            self.sync()

    def move(self, window, x, y):
        self.x.XWarpPointer(self.display, 0, window, 0, 0, 0, 0, x, y)
        self.sync()

    def button(self, button, down):
        if not self.xt.XTestFakeButtonEvent(self.display, button, int(down), 0):
            raise RuntimeError("X11 refused native button input")
        self.sync()
