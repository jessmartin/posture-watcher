import sys
import time

import badger2040


PROTOCOL = "POSTURE_WATCHER_BADGER_V1"
WIDTH = badger2040.WIDTH
HEIGHT = badger2040.HEIGHT

display = badger2040.Badger2040()
display.update_speed(badger2040.UPDATE_FAST)


def clear():
    display.pen(15)
    display.clear()
    display.pen(0)


def draw_waiting():
    clear()
    display.thickness(1)
    cy = HEIGHT // 2
    display.line(18, cy, WIDTH - 18, cy)
    display.line(18, cy - 10, 18, cy + 10)
    display.line(WIDTH - 18, cy - 10, WIDTH - 18, cy + 10)
    display.text("waiting", 8, 8, 0.45)
    display.update()


def draw_points(points, note=""):
    clear()

    # Portrait mode: place the Badger on its short edge. The body chain uses
    # the 296px axis; forward/back drift uses the 128px axis.
    cy = HEIGHT // 2
    display.thickness(1)
    display.line(18, cy, WIDTH - 18, cy)
    display.line(18, cy - 14, 18, cy + 14)
    display.line(WIDTH - 18, cy - 14, WIDTH - 18, cy + 14)

    if len(points) > 1:
        display.thickness(4)
        for i in range(len(points) - 1):
            x1, y1 = points[i]
            x2, y2 = points[i + 1]
            display.line(x1, y1, x2, y2)

    display.thickness(1)
    for x, y in points:
        display.rectangle(x - 3, y - 3, 7, 7)

    if note:
        display.text(note[:12], 8, 8, 0.4)

    display.update()


def parse_payload(line):
    parts = line.strip().split(",")
    if len(parts) < 4 or parts[0] != "P":
        return None, ""
    try:
        n = int(parts[1])
        coords = parts[2 : 2 + n * 2]
        points = []
        for i in range(0, len(coords), 2):
            x = max(0, min(WIDTH - 1, int(coords[i])))
            y = max(0, min(HEIGHT - 1, int(coords[i + 1])))
            points.append((x, y))
        note = parts[2 + n * 2] if len(parts) > 2 + n * 2 else ""
        return points, note
    except Exception:
        return None, ""


def ack(message):
    sys.stdout.write(message + "\n")
    try:
        sys.stdout.flush()
    except Exception:
        pass


draw_waiting()
ack("READY," + PROTOCOL)

while True:
    line = sys.stdin.readline()
    if not line:
        time.sleep(0.05)
        continue
    line = line.strip()
    if line == "PING":
        ack("OK," + PROTOCOL)
        continue
    points, note = parse_payload(line)
    if points:
        draw_points(points, note)
        ack("OK,P,{}".format(len(points)))
    else:
        ack("ERR,BAD_PAYLOAD")
