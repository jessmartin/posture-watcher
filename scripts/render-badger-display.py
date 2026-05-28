#!/usr/bin/env python3
"""Render the Badger receiver's actual framebuffer for visual debugging."""

import argparse
import importlib.util
import os
import struct
import sys
import types
import zlib


sys.dont_write_bytecode = True

RAW_WIDTH = 296
RAW_HEIGHT = 128
USB_TOP = "T"
USB_BOTTOM = "B"


class FakeBadger2040:
    def __init__(self):
        self.current_pen = 0
        self.current_thickness = 1
        self.pixels = [[255 for _ in range(RAW_WIDTH)] for _ in range(RAW_HEIGHT)]

    def update_speed(self, _speed):
        pass

    def pen(self, value):
        self.current_pen = int(round((max(0, min(15, value)) / 15) * 255))

    def clear(self):
        for y in range(RAW_HEIGHT):
            self.pixels[y] = [self.current_pen for _ in range(RAW_WIDTH)]

    def thickness(self, value):
        self.current_thickness = max(1, int(value))

    def rectangle(self, x, y, w, h):
        x = int(round(x))
        y = int(round(y))
        w = int(round(w))
        h = int(round(h))
        for yy in range(max(0, y), min(RAW_HEIGHT, y + h)):
            row = self.pixels[yy]
            for xx in range(max(0, x), min(RAW_WIDTH, x + w)):
                row[xx] = self.current_pen

    def line(self, x0, y0, x1, y1):
        x0 = int(round(x0))
        y0 = int(round(y0))
        x1 = int(round(x1))
        y1 = int(round(y1))
        dx = abs(x1 - x0)
        sx = 1 if x0 < x1 else -1
        dy = -abs(y1 - y0)
        sy = 1 if y0 < y1 else -1
        err = dx + dy
        while True:
            self._thick_pixel(x0, y0)
            if x0 == x1 and y0 == y1:
                break
            e2 = 2 * err
            if e2 >= dy:
                err += dy
                x0 += sx
            if e2 <= dx:
                err += dx
                y0 += sy

    def update(self):
        pass

    def _thick_pixel(self, x, y):
        size = self.current_thickness
        before = size // 2
        after = size - before
        for yy in range(y - before, y + after):
            if yy < 0 or yy >= RAW_HEIGHT:
                continue
            row = self.pixels[yy]
            for xx in range(x - before, x + after):
                if 0 <= xx < RAW_WIDTH:
                    row[xx] = self.current_pen


def install_fake_badger_module():
    module = types.ModuleType("badger2040")
    module.WIDTH = RAW_WIDTH
    module.HEIGHT = RAW_HEIGHT
    module.UPDATE_FAST = 2
    module.Badger2040 = FakeBadger2040
    sys.modules["badger2040"] = module


def load_receiver(repo_root):
    install_fake_badger_module()
    receiver_path = os.path.join(repo_root, "badger", "posture_receiver.py")
    spec = importlib.util.spec_from_file_location("posture_receiver_for_render", receiver_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load Badger receiver")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def clean_payload(payload):
    payload = payload.strip()
    if "DISPLAY," in payload:
        payload = payload[payload.index("DISPLAY,") + len("DISPLAY,") :]
    return payload.strip()


def latest_payload_from_log(path):
    latest = None
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if "DISPLAY," in line:
                latest = clean_payload(line)
    if latest is None:
        raise RuntimeError(f"no DISPLAY payload found in {path}")
    return latest


def payload_orientation(payload):
    parts = payload.strip().split(",")
    if len(parts) > 1 and parts[1] in (USB_TOP, USB_BOTTOM):
        return parts[1]
    return USB_TOP


def render_payload(receiver, payload):
    kind, orientation, points, baseline_points, quality_bits, note = receiver.parse_payload(payload)
    if kind == "M":
        receiver.draw_message(note, orientation)
    elif kind == "P" and points:
        receiver.draw_points(points, note, orientation, baseline_points, quality_bits)
    else:
        raise RuntimeError(f"bad payload: {payload}")
    return receiver.display.pixels, orientation


def mounted_pixels(raw_pixels, orientation):
    mounted = [[255 for _ in range(RAW_HEIGHT)] for _ in range(RAW_WIDTH)]
    for y in range(RAW_WIDTH):
        for x in range(RAW_HEIGHT):
            if orientation == USB_BOTTOM:
                raw_x = RAW_WIDTH - 1 - y
                raw_y = x
            else:
                raw_x = y
                raw_y = RAW_HEIGHT - 1 - x
            mounted[y][x] = raw_pixels[raw_y][raw_x]
    return mounted


def write_png(path, pixels, scale):
    height = len(pixels)
    width = len(pixels[0]) if height else 0
    scaled_width = width * scale
    scaled_height = height * scale
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)

    rows = []
    for row in pixels:
        rgb = bytearray()
        for value in row:
            pixel = int(max(0, min(255, value)))
            rgb.extend([pixel, pixel, pixel])
        expanded = bytes(rgb)
        for _ in range(scale):
            line = bytearray()
            for x in range(width):
                line.extend(expanded[x * 3 : x * 3 + 3] * scale)
            rows.append(b"\x00" + bytes(line))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    png = bytearray()
    png.extend(b"\x89PNG\r\n\x1a\n")
    png.extend(chunk(b"IHDR", struct.pack(">IIBBBBB", scaled_width, scaled_height, 8, 2, 0, 0, 0)))
    png.extend(chunk(b"IDAT", zlib.compress(b"".join(rows), 9)))
    png.extend(chunk(b"IEND", b""))
    with open(path, "wb") as handle:
        handle.write(png)


def default_log_path():
    return os.path.expanduser("~/Library/Application Support/Posture Watcher/posture-watcher.log")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", help="DISPLAY payload, with or without the DISPLAY, prefix")
    parser.add_argument(
        "--log",
        default=default_log_path(),
        help="App log to read when --payload is omitted",
    )
    parser.add_argument(
        "--out",
        default="artifacts/badger-debug/latest-mounted.png",
        help="PNG path for the mounted portrait view",
    )
    parser.add_argument("--raw-out", help="Optional PNG path for the native 296x128 framebuffer")
    parser.add_argument("--scale", type=int, default=3, help="Integer PNG scale factor")
    args = parser.parse_args()

    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    payload = clean_payload(args.payload) if args.payload else latest_payload_from_log(args.log)
    receiver = load_receiver(repo_root)
    raw_pixels, orientation = render_payload(receiver, payload)
    write_png(args.out, mounted_pixels(raw_pixels, orientation), max(1, args.scale))
    if args.raw_out:
        write_png(args.raw_out, raw_pixels, max(1, args.scale))
    print(f"payload={payload}")
    print(f"orientation={'usb-bottom' if orientation == USB_BOTTOM else 'usb-top'}")
    print(f"mounted={args.out}")
    if args.raw_out:
        print(f"raw={args.raw_out}")


if __name__ == "__main__":
    main()
