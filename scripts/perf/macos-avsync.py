#!/usr/bin/env python3
"""Measure the A/V sync of a video playing in a Kettle lane on macOS.

  macos-avsync.py run --kettle BIN --worker BIN [--runs 3] [--out DIR]
      For each run, launch a Kettle of its own with an isolated
      configuration, show the fixture on its pane's shelf, open the lane,
      and measure: stream the lane's picture and the system's sound through
      avsync-probe, press the lane's play control natively, give the lane the
      keyboard through ctl, press Left natively at 10, 16 and 22 s (back five
      seconds; a control client may neither start nor step a video), and
      judge the capture. Then close that Kettle. The worker is staged beside
      a copy of Kettle and signed with the machine's one Developer ID
      Application identity (--sign-identity names another), since Kettle
      runs only a worker its own requirement accepts. Results go to
      target/perf-results/avsync/<UTC time>/; each capture's sound, which is
      every app's, is deleted once judged. Nothing else may play meanwhile,
      the fixture's clicks are heard, and the window must stay uncovered.

  macos-avsync.py fixture OUT.mov
      Write the flash/click fixture with ffmpeg: 40 s of black at 30 fps, a
      white frame pair and a 20 ms 1 kHz tone at 0.5 s past each second,
      H.264 with a key frame each second, and 48 kHz PCM sound, in QuickTime.

  macos-avsync.py analyze DIR [--play-ns T] [--seek-ns T1,T2,...]
      Judge a capture avsync-probe wrote to DIR.

  macos-avsync.py --self-test
      The analysis on synthetic captures.

The sync error is E = t_video_display - t_audio_output for each flash and
its click; positive E means the sound is early. The target is -125 ms <= E
<= +45 ms (ITU-R BT.1359-1's detectability thresholds), held throughout
steady playback after the start and after each seek. Each E is a band, not
a point. A flash's time is when the window server displayed the frame
(ScreenCaptureKit's displayTime; the panel's own response is not included).
A click's time is when the system mix ScreenCaptureKit taps had it, plus
the output device's latency and stream latency. That tap time lies between
the moment the mix was made and its I/O time, which is the safety offset and
the I/O buffer later. So each E spans those two frame counts at the
device's rate, and a pass needs the whole band inside the target. Both legs
are host time, and the probe refuses a capture whose timestamps stray from
it.

At 30 frames a second on a 60 Hz display, each flash meets the same phase of
the display's refresh until the next seek, so a run samples one phase per
segment; several runs and seeks sample the spread.

The probe streams the screen through ScreenCaptureKit, so whatever runs it
needs Screen Recording; the terminal it is started from lends it its grant.
Standard library only.
"""

from __future__ import annotations

import argparse
import array
import datetime
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

TARGET_MS = (-125.0, 45.0)
PAIR_WINDOW_NS = 450_000_000
CLICK_REFRACTORY_NS = 300_000_000
MIN_STEADY_PAIRS = 5
FIXTURE_SECONDS = 40
LEFT_ARROW = 123  # the virtual key code
CAPTURE_SECONDS = 30
SEEKS_AT = (10.0, 16.0, 22.0)
REPO = Path(__file__).resolve().parents[2]
PROBE_SOURCE = Path(__file__).resolve().parent / "macos-avsync" / "avsync-probe.swift"
WORKER_IDENTIFIER = "org.kettle.terminal.media-worker"
CONFIG = """restore-session = false
update-check = false
record = off
window-width = 120
window-height = 44
agent-server = full
agent-display = true
media-open-mode = lane
"""


def uptime_ns() -> int:
    """Host time in nanoseconds: mach_absolute_time, the probe's clock."""
    return time.clock_gettime_ns(time.CLOCK_UPTIME_RAW)


# ---------------------------------------------------------------------------
# Fixture


def fixture_command(ffmpeg: str, out: Path) -> List[str]:
    video = (
        f"color=c=black:s=640x360:r=30:d={FIXTURE_SECONDS},format=yuv420p,"
        "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:"
        "enable='between(mod(n\\,30)\\,15\\,16)'"
    )
    # The tone starts at a zero crossing, rising, on the flash's first frame.
    audio = (
        "aevalsrc='if(between(mod(t\\,1)\\,0.5\\,0.52)\\,"
        "0.5*sin(2*PI*1000*(mod(t\\,1)-0.5))\\,0)'"
        f":s=48000:d={FIXTURE_SECONDS}:c=stereo"
    )
    return [
        ffmpeg, "-v", "error", "-nostdin", "-n",
        "-f", "lavfi", "-i", video,
        "-f", "lavfi", "-i", audio,
        "-c:v", "libx264", "-g", "30", "-keyint_min", "30", "-bf", "0",
        "-c:a", "pcm_s16le", "-shortest", "-f", "mov", str(out),
    ]


def make_fixture(out: Path) -> None:
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        sys.exit("ffmpeg not found")
    subprocess.run(fixture_command(ffmpeg, out), check=True, stdin=subprocess.DEVNULL, timeout=120)


# ---------------------------------------------------------------------------
# Analysis


@dataclass
class Capture:
    frames: List[Tuple[int, float]]
    samples: Sequence[float]
    buffers: List[Tuple[int, int, int]]  # pts_ns, samples, offset
    rate: float
    run: Dict


def load(directory: Path) -> Capture:
    error = directory / "error.json"
    if error.exists():
        raise ValueError("the probe failed: " + json.loads(error.read_text()).get("error", "?"))
    frames = [(int(f["display_ns"]), float(f["luma"]))
              for f in json.loads((directory / "frames.json").read_text())["frames"]]
    audio = json.loads((directory / "audio.json").read_text())
    samples = array.array("f")
    samples.frombytes((directory / "audio.f32").read_bytes())
    if sys.byteorder != "little":
        samples.byteswap()
    buffers = [(int(b["pts_ns"]), int(b["samples"]), int(b["offset"])) for b in audio["buffers"]]
    return Capture(sorted(frames), samples, buffers, float(audio["sample_rate"]),
                   json.loads((directory / "run.json").read_text()))


def percentile(values: Sequence[float], p: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("no values")
    return ordered[min(len(ordered) - 1, max(0, int(round(p / 100 * (len(ordered) - 1)))))]


def flash_onsets(frames: Sequence[Tuple[int, float]]) -> Tuple[List[int], float, float]:
    """The display time of each frame where the picture turns bright, with
    the dark and bright levels the threshold halves."""
    lumas = [luma for _, luma in frames]
    dark, bright = percentile(lumas, 5), percentile(lumas, 99.5)
    if bright - dark < 0.2:
        raise ValueError(f"no flash seen (luma {dark:.3f} to {bright:.3f})")
    threshold = (dark + bright) / 2
    onsets: List[int] = []
    lit = False
    for ns, luma in frames:
        if not lit and luma >= threshold:
            onsets.append(ns)
            lit = True
        elif lit and luma < threshold:
            lit = False
    return onsets, dark, bright


def audio_gaps_ns(buffers: Sequence[Tuple[int, int, int]], rate: float) -> int:
    """The largest difference between where a buffer starts and where the one
    before it said it would."""
    worst = 0
    for (pts, count, _), (next_pts, _, _) in zip(buffers, buffers[1:]):
        worst = max(worst, abs(next_pts - (pts + round(count * 1e9 / rate))))
    return worst


def click_onsets(samples: Sequence[float], buffers: Sequence[Tuple[int, int, int]],
                 rate: float) -> Tuple[List[int], float]:
    """The time of each sample that starts a click, a sample at least 30 % of
    the loudest after 300 ms below that, and the loudest level."""
    if not samples:
        raise ValueError("no sound captured")
    loudest = percentile(samples, 99.99)
    if loudest < 0.01:
        raise ValueError(f"no click heard (peak {loudest:.4f}; muted, or nothing playing?)")
    threshold = 0.3 * loudest
    onsets: List[int] = []
    last_loud: Optional[int] = None
    for pts, count, offset in buffers:
        for i in range(count):
            if samples[offset + i] < threshold:
                continue
            at = pts + round(i * 1e9 / rate)
            if last_loud is None or at - last_loud >= CLICK_REFRACTORY_NS:
                onsets.append(at)
            last_loud = at
    return onsets, loudest


def pair(flashes: Sequence[int], clicks: Sequence[int]) -> Tuple[List[Tuple[int, int]], List[int]]:
    """Each flash with the nearest click within 450 ms, and the flashes with
    none."""
    pairs, lone = [], []
    for flash in flashes:
        near = min(clicks, key=lambda click: abs(click - flash), default=None)
        if near is not None and abs(near - flash) <= PAIR_WINDOW_NS:
            pairs.append((flash, near))
        else:
            lone.append(flash)
    return pairs, lone


def latency_parts_ns(latency: Dict) -> Tuple[float, float]:
    """(always added, the band's width) in ns: device and stream latency,
    and the safety offset and I/O buffer."""
    rate = latency.get("nominal_rate")
    if not rate or not all(k in latency for k in ("device_latency_frames", "safety_offset_frames", "buffer_frames")):
        raise ValueError(f"the output device's latency is unknown: {latency}")
    fixed = latency["device_latency_frames"] + latency.get("stream_latency_frames", 0)
    width = latency["safety_offset_frames"] + latency["buffer_frames"]
    return fixed * 1e9 / rate, width * 1e9 / rate


@dataclass
class Sample:
    flash_ns: int
    low_ms: float
    high_ms: float

    def inside(self) -> bool:
        return TARGET_MS[0] <= self.low_ms and self.high_ms <= TARGET_MS[1]


def judge(capture: Capture, play_ns: Optional[int], seeks_ns: Sequence[int] = ()) -> Dict:
    flashes, dark, bright = flash_onsets(capture.frames)
    clicks, loudest = click_onsets(capture.samples, capture.buffers, capture.rate)
    pairs, lone = pair(flashes, clicks)
    latency = capture.run["output_latency"]
    if "changed_to" in latency:
        raise ValueError("the output device changed during the capture")
    fixed, width = latency_parts_ns(latency)
    samples = []
    for flash, click in pairs:
        high = (flash - click - fixed) / 1e6
        samples.append(Sample(flash, high - width / 1e6, high))
    events = ([("play", play_ns)] if play_ns is not None else [("capture", capture.frames[0][0])])
    events += [(f"seek {i + 1}", ns) for i, ns in enumerate(seeks_ns)]
    segments = []
    for i, (label, start) in enumerate(events):
        end = events[i + 1][1] if i + 1 < len(events) else math.inf
        inside = [s for s in samples if start <= s.flash_ns < end]
        unpaired = [f for f in lone if start <= f < end]
        # Recovery: the first sample from which every later one is inside.
        recovered = None
        for j in range(len(inside)):
            if all(s.inside() for s in inside[j:]):
                recovered = j
                break
        steady = inside[recovered:] if recovered is not None else []
        segment = {
            "from": label,
            "pairs": len(inside),
            "unpaired_flashes": len(unpaired),
            "recovery_ms": None if recovered is None else round((inside[recovered].flash_ns - start) / 1e6, 1),
            "steady_pairs": len(steady),
            "steady_low_ms": round(min(s.low_ms for s in steady), 1) if steady else None,
            "steady_high_ms": round(max(s.high_ms for s in steady), 1) if steady else None,
            "steady_mean_ms": round(sum((s.low_ms + s.high_ms) / 2 for s in steady) / len(steady), 1) if steady else None,
            "before_recovery": [[round(s.low_ms, 1), round(s.high_ms, 1)] for s in inside[: recovered or 0]],
            # Every pair's band, in order: the spread, not just its extremes.
            "bands_ms": [[round(s.low_ms, 1), round(s.high_ms, 1)] for s in inside],
            "first_pair_ms": round((inside[0].flash_ns - start) / 1e6, 1) if inside else None,
        }
        segment["pass"] = (
            len(steady) >= MIN_STEADY_PAIRS
            and not [f for f in unpaired if recovered is not None and f >= inside[recovered].flash_ns]
        )
        segments.append(segment)
    return {
        "target_ms": list(TARGET_MS),
        "band_width_ms": round(width / 1e6, 2),
        "added_latency_ms": round(fixed / 1e6, 2),
        "flashes": len(flashes),
        "clicks": len(clicks),
        "luma": [round(dark, 3), round(bright, 3)],
        "loudest": round(loudest, 4),
        "audio_gap_ms": round(audio_gaps_ns(capture.buffers, capture.rate) / 1e6, 2),
        "frames": len(capture.frames),
        "segments": segments,
        "pass": bool(segments) and all(s["pass"] for s in segments),
    }


# ---------------------------------------------------------------------------
# Run


def ctl(kettle: str, pid: int, *args: str) -> Dict:
    out = subprocess.run([kettle, "ctl", "--pid", str(pid), *args], check=True,
                         capture_output=True, text=True, timeout=30, stdin=subprocess.DEVNULL)
    return json.loads(out.stdout) if out.stdout.strip().startswith("{") else {}


def lane_target(geometry: Dict) -> Tuple[Tuple[float, float, float, float], Tuple[float, float], Dict]:
    """The lane image's rectangle and its play control's centre, in global
    points, and the lane's playback ({} before it first plays)."""
    lanes = [lane for lane in geometry.get("preview_lanes", [])
             if "play" in (lane.get("controls") or {})]
    if len(lanes) != 1:
        raise ValueError(f"want exactly one lane with a video, found {len(lanes)}")
    lane = lanes[0]
    origin = geometry.get("desktop", {}).get("client_origin")
    scale = geometry.get("scale_factor") or 1.0
    if not origin or not lane.get("image"):
        raise ValueError("the lane's image or the window origin is not reported")
    ox, oy = origin["x"] / scale, origin["y"] / scale
    image, play = lane["image"], lane["controls"]["play"]
    rect = (ox + image["x"], oy + image["y"], image["width"], image["height"])
    centre = (ox + play["x"] + play["width"] / 2, oy + play["y"] + play["height"] / 2)
    return rect, centre, lane.get("playback") or {}


def raise_window(pid: int) -> None:
    script = f'tell application "System Events" to set frontmost of (first process whose unix id is {pid}) to true'
    subprocess.run(["osascript", "-e", script], check=False, capture_output=True, timeout=10)
    time.sleep(0.5)
    front = subprocess.run(
        ["osascript", "-e", 'tell application "System Events" to get unix id of first process whose frontmost is true'],
        capture_output=True, text=True, timeout=10).stdout.strip()
    if front != str(pid):
        raise RuntimeError(f"Kettle is not frontmost (pid {front} is)")


def wait_for(what: str, check, timeout_s: float = 10.0):
    deadline = time.monotonic() + timeout_s
    last: object = None
    while time.monotonic() < deadline:
        try:
            last = check()
            if last:
                return last
        except (subprocess.SubprocessError, ValueError, KeyError) as error:
            last = error
        time.sleep(0.2)
    raise RuntimeError(f"timed out waiting for {what} ({last!r})")


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_probe(tools: Path) -> Path:
    """avsync-probe, compiled again whenever its source changes."""
    probe = tools / "avsync-probe"
    stamp = tools / "avsync-probe.source-sha256"
    digest = file_sha256(PROBE_SOURCE)
    if not (probe.exists() and stamp.exists() and stamp.read_text().strip() == digest):
        swiftc = shutil.which("swiftc")
        if not swiftc:
            raise RuntimeError("swiftc not found")
        subprocess.run([swiftc, "-O", "-swift-version", "5", "-o", str(probe), str(PROBE_SOURCE)],
                       check=True, stdin=subprocess.DEVNULL, timeout=600)
        stamp.write_text(digest + "\n")
    if subprocess.run([str(probe), "--self-test"], stdin=subprocess.DEVNULL, timeout=60).returncode:
        raise RuntimeError("avsync-probe's self-test failed")
    return probe


def signing_identity() -> str:
    found = subprocess.run(["security", "find-identity", "-v", "-p", "codesigning"],
                           check=True, capture_output=True, text=True, timeout=30).stdout
    chosen = subprocess.run([sys.executable, str(REPO / "scripts" / "select-macos-signing-identity.py")],
                            input=found, capture_output=True, text=True, timeout=30)
    if chosen.returncode:
        raise RuntimeError(chosen.stderr.strip() + "; name one with --sign-identity")
    return chosen.stdout.strip()


def stage(kettle: Path, worker: Path, tools: Path, identity: str) -> Path:
    """A copy of Kettle with the worker beside it, signed as Kettle's own
    requirement asks: Developer ID, the worker's identifier, the hardened
    runtime."""
    bin_dir = tools / "bin"
    bin_dir.mkdir(mode=0o700, exist_ok=True)
    staged = bin_dir / "kettle"
    shutil.copy2(kettle, staged)
    shutil.copy2(worker, bin_dir / "kettle-media-worker")
    subprocess.run(["codesign", "--force", "--options", "runtime", "--identifier", WORKER_IDENTIFIER,
                    "--sign", identity, str(bin_dir / "kettle-media-worker")],
                   check=True, capture_output=True, timeout=120)
    return staged


def in_gui_session() -> bool:
    out = subprocess.run(["launchctl", "managername"], capture_output=True, text=True, timeout=10)
    return out.stdout.strip() == "Aqua"


def launch(kettle: Path, home: Path) -> int:
    """Start a Kettle of its own with the isolated configuration; its pid."""
    config = home / "config"
    config.write_text(CONFIG)
    pid_file = home / "kettle.pid"
    command = ["/bin/sh", "-c", 'echo $$ > "$0"; exec "$1" --new-process --config "$2"',
               str(pid_file), str(kettle), str(config)]
    if not in_gui_session():
        # A shell outside the GUI session reaches it through launchd.
        command = ["launchctl", "asuser", str(os.getuid()), *command]
    log = open(home / "kettle.log", "ab")
    subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    pid = int(wait_for("Kettle's pid", lambda: pid_file.exists() and pid_file.read_text().strip()))
    wait_for("Kettle's control server", lambda: ctl(str(kettle), pid, "ui_geometry", "--raw"))
    return pid


def close(kettle: Path, pid: int) -> None:
    """Close the Kettle `launch` started, by its recorded pid alone."""
    try:
        ctl(str(kettle), pid, "perform_action", "--text", "close_window")
    except subprocess.SubprocessError:
        pass
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.2)
    os.kill(pid, 15)


def prepare(kettle: Path, pid: int, fixture: Path) -> None:
    k = str(kettle)
    wait_for("the media worker",
             lambda: (ctl(k, pid, "get_state", "--raw").get("media") or {}).get("availability") == "available")
    ctl(k, pid, "send_text", "--text", f"{k} show {fixture}\n")
    wait_for("the fixture on the shelf",
             lambda: "Sent to the Kettle media shelf" in json.dumps(ctl(k, pid, "read_screen", "--raw")))
    ctl(k, pid, "perform_action", "--text", "open_media_shelf")
    wait_for("the fixture's lane", lambda: lane_target(ctl(k, pid, "ui_geometry", "--raw")))


def measure(kettle: Path, pid: int, probe: Path, out: Path) -> Dict:
    """One capture of the lane, from its play press through three seeks."""
    k = str(kettle)
    rect, centre, playback = lane_target(ctl(k, pid, "ui_geometry", "--raw"))
    if playback.get("state") == "playing" or playback.get("position_ms", 0) != 0:
        raise RuntimeError(f"the video must be at its start, not playing: {playback}")
    raise_window(pid)
    capture = out / "capture"
    capture.mkdir(mode=0o700)
    probe_run = subprocess.Popen(
        [str(probe), "--rect", ",".join(f"{v:.1f}" for v in rect), "--seconds", str(CAPTURE_SECONDS),
         "--out", str(capture), "--click", f"{centre[0]:.1f},{centre[1]:.1f}", "--click-after", "1.5",
         "--key", str(LEFT_ARROW), "--key-at", ",".join(f"{t:g}" for t in SEEKS_AT)],
        stdin=subprocess.DEVNULL)
    try:
        time.sleep(3.0)
        heard = lane_target(ctl(k, pid, "ui_geometry", "--raw"))[2]
        if heard.get("state") != "playing" or heard.get("sound") != "heard":
            raise RuntimeError(f"the press did not start the video with its sound: {heard}")
        # The lane takes the keyboard for the arrows to reach it.
        ctl(k, pid, "perform_action", "--text", "focus_preview")
        probe_run.wait(timeout=CAPTURE_SECONDS + 30)
    finally:
        if probe_run.poll() is None:
            probe_run.terminate()
            probe_run.wait(timeout=10)
    if probe_run.returncode != 0:
        raise RuntimeError(f"avsync-probe exited {probe_run.returncode}")
    loaded = load(capture)
    if len(loaded.run.get("keys_ns") or []) != len(SEEKS_AT):
        raise RuntimeError("not every seek was pressed")
    result = judge(loaded, loaded.run.get("click_ns"), loaded.run["keys_ns"])
    result["output_latency"] = loaded.run["output_latency"]
    # The sound was every app's: keep only what was judged.
    (capture / "audio.f32").unlink()
    (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def summarize(results: Sequence[Dict]) -> Dict:
    segments = [segment for result in results for segment in result["segments"]]
    steady = [segment for segment in segments if segment["steady_pairs"]]
    return {
        "runs": len(results),
        "segments": len(segments),
        "pairs": sum(segment["pairs"] for segment in segments),
        "out_of_target_pairs": sum(len(segment["before_recovery"]) for segment in segments),
        "steady_low_ms": min((s["steady_low_ms"] for s in steady), default=None),
        "steady_high_ms": max((s["steady_high_ms"] for s in steady), default=None),
        "worst_recovery_ms": max((s["recovery_ms"] for s in steady), default=None),
        "pass": bool(results) and all(result["pass"] for result in results),
    }


def run(args: argparse.Namespace) -> int:
    out = Path(args.out) if args.out else (
        REPO / "target" / "perf-results" / "avsync"
        / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ"))
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    tools = REPO / "target" / "perf-tools" / "avsync"
    tools.mkdir(mode=0o700, parents=True, exist_ok=True)
    probe = build_probe(tools)
    kettle = stage(Path(args.kettle), Path(args.worker), tools, args.sign_identity or signing_identity())
    results = []
    with tempfile.TemporaryDirectory(prefix="kettle-avsync-") as private:
        private_dir = Path(private)
        fixture = private_dir / "flash-click.mov"
        make_fixture(fixture)
        for index in range(args.runs):
            home = private_dir / f"run-{index + 1}"
            home.mkdir(mode=0o700)
            pid = launch(kettle, home)
            try:
                prepare(kettle, pid, fixture)
                run_dir = out / f"run-{index + 1}"
                run_dir.mkdir(mode=0o700)
                results.append(measure(kettle, pid, probe, run_dir))
            finally:
                close(kettle, pid)
    summary = summarize(results)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    print(f"results: {out}")
    return 0 if summary["pass"] else 1


# ---------------------------------------------------------------------------
# Self-test


def synthetic(offsets_ms: Sequence[float], frame_ns: int = 8_333_333, rate: float = 48_000.0,
              latency: Optional[Dict] = None, mute: bool = False) -> Capture:
    """Flashes each second from 1.5 s; each click lands offsets_ms[i] after
    its flash (negative: before), before device latency."""
    start = 10_000_000_000
    frames, samples, buffers = [], array.array("f"), []
    seconds = len(offsets_ms) + 2
    # Each flash on a frame, as a display shows it.
    flash_times = [start + round((k + 1.5) * 1e9 / frame_ns) * frame_ns for k in range(len(offsets_ms))]
    t = start
    while t < start + seconds * 1_000_000_000:
        lit = any(f <= t < f + 66_000_000 for f in flash_times)
        frames.append((t, 0.95 if lit else 0.05))
        t += frame_ns
    click_starts = [f + int(o * 1e6) for f, o in zip(flash_times, offsets_ms)]
    per = 960
    n = int(seconds * rate)
    for b in range(0, n, per):
        pts = start + round(b * 1e9 / rate)
        buffers.append((pts, per, len(samples)))
        for i in range(per):
            at = pts + round(i * 1e9 / rate)
            loud = not mute and any(c <= at < c + 20_000_000 for c in click_starts)
            samples.append(0.5 if loud else 0.0)
    latency = latency or {"device_latency_frames": 48, "stream_latency_frames": 0,
                          "safety_offset_frames": 48, "buffer_frames": 480, "nominal_rate": 48000.0}
    return Capture(frames, samples, buffers, rate, {"output_latency": latency})


def self_test() -> int:
    failures = []

    def check(ok: bool, what: str) -> None:
        if not ok:
            failures.append(what)

    # In sync: the click is in the tap 1 ms (device latency) before the flash
    # shows, so E spans -11 ms to -0 ms.
    capture = synthetic([-1.0] * 12)
    result = judge(capture, capture.frames[0][0])
    seg = result["segments"][0]
    check(result["pass"], f"in sync passes: {result}")
    check(seg["pairs"] == 12 and seg["recovery_ms"] is not None, f"every flash pairs: {seg}")
    check(abs(seg["steady_high_ms"] - 0.0) <= 0.1 and abs(seg["steady_low_ms"] + 11.0) <= 0.2,
          f"the band is the safety offset and buffer: {seg}")
    # Sound 200 ms late: E = +199, outside the band; never recovers.
    late = synthetic([-200.0] * 10)
    check(not judge(late, late.frames[0][0])["pass"], "sound 200 ms early fails")
    early = synthetic([150.0] * 10)
    check(not judge(early, early.frames[0][0])["pass"], "sound 150 ms late fails")
    # Edges: +45 at the band's top passes; just past it fails.
    edge = synthetic([-46.0] * 10)
    check(judge(edge, edge.frames[0][0])["pass"], "E = +45 at the top passes")
    over = synthetic([-47.0] * 10)
    check(not judge(over, over.frames[0][0])["pass"], "E = +46 fails")
    # Band -124.97 .. -113.97: a click is found up to a sample (21 us) late.
    low = synthetic([112.97] * 10)
    check(judge(low, low.frames[0][0])["pass"], "E = -125 at the bottom passes")
    lower = synthetic([114.0] * 10)
    check(not judge(lower, lower.frames[0][0])["pass"], "E = -126 fails")
    # Recovery after a seek: three bad pairs, then good ones.
    offsets = [-1.0] * 8 + [-300.0] * 3 + [-1.0] * 8
    seek = synthetic(offsets)
    seek_ns = 10_000_000_000 + int(9.2 * 1e9)
    result = judge(seek, seek.frames[0][0], [seek_ns])
    after = result["segments"][1]
    check(result["pass"], f"recovers after the seek: {result}")
    check(after["from"] == "seek 1" and len(after["before_recovery"]) == 3, f"three before recovery: {after}")
    check(abs(after["recovery_ms"] - (12.5 * 1e3 - 9.2 * 1e3)) < 10, f"recovery time from the seek: {after}")
    # Too few steady pairs after a seek fails.
    short = synthetic([-1.0] * 8 + [-300.0] * 8 + [-1.0] * 2)
    check(not judge(short, short.frames[0][0], [seek_ns])["pass"], "two steady pairs are too few")
    # A dropped click after recovery fails.
    drop = synthetic([-1.0] * 12)
    lone_flash = drop.frames[0][0] + int(6.5 * 1e9)
    for b, (pts, count, offset) in enumerate(drop.buffers):
        for i in range(count):
            at = pts + round(i * 1e9 / drop.rate)
            if lone_flash - 10_000_000 <= at < lone_flash + 30_000_000:
                drop.samples[offset + i] = 0.0
    check(not judge(drop, drop.frames[0][0])["pass"], "a flash with no click fails")
    # Muted or not playing: refused, never a pass.
    for bad, why in ((synthetic([-1.0] * 5, mute=True), "no click"),):
        try:
            judge(bad, None)
            failures.append(f"{why} must be refused")
        except ValueError:
            pass
    # Unknown latency is refused.
    try:
        judge(synthetic([-1.0] * 6, latency={"nominal_rate": 48000.0}), None)
        failures.append("unknown latency must be refused")
    except ValueError:
        pass
    changed = synthetic([-1.0] * 6)
    changed.run["output_latency"]["changed_to"] = {}
    try:
        judge(changed, None)
        failures.append("a changed device must be refused")
    except ValueError:
        pass
    # Pairing: a click 500 ms away is not a flash's.
    pairs, lone = pair([1_000_000_000], [1_500_000_001])
    check(not pairs and lone == [1_000_000_000], "a click past 450 ms does not pair")
    # Audio gaps are measured.
    check(audio_gaps_ns([(0, 480, 0), (10_000_000, 480, 480), (25_000_000, 480, 960)], 48_000.0) == 5_000_000,
          "a 5 ms gap is found")
    # The seek schedule leaves each segment its steady pairs, inside the
    # fixture: Left steps back, so the video never runs out.
    times = [1.5, *SEEKS_AT, CAPTURE_SECONDS]
    check(all(b - a >= MIN_STEADY_PAIRS + 1 for a, b in zip(times, times[1:])), "seek schedule")
    check(1.5 + CAPTURE_SECONDS <= FIXTURE_SECONDS, "the capture fits the fixture")
    # A lane before its first play has no playback; its rectangles are
    # read in global points from the client origin.
    geometry = {
        "scale_factor": 2.0,
        "desktop": {"client_origin": {"x": 200, "y": 100}},
        "preview_lanes": [
            {"image": {"x": 10, "y": 20, "width": 300, "height": 150},
             "controls": {"play": {"x": 40, "y": 5, "width": 20, "height": 10}}},
            {"image": {"x": 0, "y": 0, "width": 1, "height": 1}, "controls": {"close": {}}},
        ],
    }
    rect, centre, playback = lane_target(geometry)
    check(rect == (110.0, 70.0, 300, 150) and centre == (150.0, 60.0) and playback == {},
          f"lane target: {rect} {centre} {playback}")
    geometry["preview_lanes"].append(geometry["preview_lanes"][0])
    try:
        lane_target(geometry)
        failures.append("two video lanes must be refused")
    except ValueError:
        pass
    # The fixture command is well formed.
    command = fixture_command("ffmpeg", Path("x.mov"))
    check("-n" in command and command[-1] == "x.mov" and "pcm_s16le" in command, "fixture command")
    for failure in failures:
        print("self-test:", failure, file=sys.stderr)
    if not failures:
        print("self-test: ok")
    return 1 if failures else 0


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    fixture = sub.add_parser("fixture")
    fixture.add_argument("out")
    runner = sub.add_parser("run")
    runner.add_argument("--kettle", required=True, help="the kettle executable to measure")
    runner.add_argument("--worker", required=True, help="its kettle-media-worker")
    runner.add_argument("--runs", type=int, default=3)
    runner.add_argument("--out", help="a new directory for the results")
    runner.add_argument("--sign-identity", help="the Developer ID Application identity to sign the worker with")
    analyze = sub.add_parser("analyze")
    analyze.add_argument("dir")
    analyze.add_argument("--play-ns", type=int)
    analyze.add_argument("--seek-ns", help="host-clock ns of each seek, separated by commas")
    args = parser.parse_args()
    if args.command == "fixture":
        make_fixture(Path(args.out))
        return 0
    if args.command == "run":
        if not 1 <= args.runs <= 20:
            parser.error("--runs takes 1 to 20")
        return run(args)
    capture = load(Path(args.dir))
    result = judge(capture, args.play_ns if args.play_ns is not None else capture.run.get("click_ns"),
                   [int(t) for t in args.seek_ns.split(",")] if args.seek_ns else capture.run.get("keys_ns", []))
    print(json.dumps(result, indent=2))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
