import sys
import time

import badger2040


PROTOCOL = "POSTURE_WATCHER_BADGER_V2"
WIDTH = badger2040.WIDTH
HEIGHT = badger2040.HEIGHT
USB_TOP = "T"
USB_BOTTOM = "B"

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
    draw_centered_text("WAITING", 52, 3, USB_TOP)
    display.update()


PIXEL_FONT = {
    "A": ["01110", "10001", "10001", "11111", "10001", "10001", "10001"],
    "B": ["11110", "10001", "10001", "11110", "10001", "10001", "11110"],
    "C": ["01111", "10000", "10000", "10000", "10000", "10000", "01111"],
    "D": ["11110", "10001", "10001", "10001", "10001", "10001", "11110"],
    "E": ["11111", "10000", "10000", "11110", "10000", "10000", "11111"],
    "F": ["11111", "10000", "10000", "11110", "10000", "10000", "10000"],
    "G": ["01111", "10000", "10000", "10011", "10001", "10001", "01110"],
    "H": ["10001", "10001", "10001", "11111", "10001", "10001", "10001"],
    "I": ["11111", "00100", "00100", "00100", "00100", "00100", "11111"],
    "J": ["00111", "00010", "00010", "00010", "10010", "10010", "01100"],
    "K": ["10001", "10010", "10100", "11000", "10100", "10010", "10001"],
    "L": ["10000", "10000", "10000", "10000", "10000", "10000", "11111"],
    "M": ["10001", "11011", "10101", "10101", "10001", "10001", "10001"],
    "N": ["10001", "11001", "10101", "10011", "10001", "10001", "10001"],
    "O": ["01110", "10001", "10001", "10001", "10001", "10001", "01110"],
    "P": ["11110", "10001", "10001", "11110", "10000", "10000", "10000"],
    "Q": ["01110", "10001", "10001", "10001", "10101", "10010", "01101"],
    "R": ["11110", "10001", "10001", "11110", "10100", "10010", "10001"],
    "S": ["01111", "10000", "10000", "01110", "00001", "00001", "11110"],
    "T": ["11111", "00100", "00100", "00100", "00100", "00100", "00100"],
    "U": ["10001", "10001", "10001", "10001", "10001", "10001", "01110"],
    "V": ["10001", "10001", "10001", "10001", "10001", "01010", "00100"],
    "W": ["10001", "10001", "10001", "10101", "10101", "10101", "01010"],
    "X": ["10001", "10001", "01010", "00100", "01010", "10001", "10001"],
    "Y": ["10001", "10001", "01010", "00100", "00100", "00100", "00100"],
    "Z": ["11111", "00001", "00010", "00100", "01000", "10000", "11111"],
    "0": ["01110", "10001", "10011", "10101", "11001", "10001", "01110"],
    "1": ["00100", "01100", "00100", "00100", "00100", "00100", "01110"],
    "2": ["01110", "10001", "00001", "00010", "00100", "01000", "11111"],
    "3": ["11110", "00001", "00001", "01110", "00001", "00001", "11110"],
    "4": ["00010", "00110", "01010", "10010", "11111", "00010", "00010"],
    "5": ["11111", "10000", "10000", "11110", "00001", "00001", "11110"],
    "6": ["01110", "10000", "10000", "11110", "10001", "10001", "01110"],
    "7": ["11111", "00001", "00010", "00100", "01000", "01000", "01000"],
    "8": ["01110", "10001", "10001", "01110", "10001", "10001", "01110"],
    "9": ["01110", "10001", "10001", "01111", "00001", "00001", "01110"],
    "+": ["00000", "00100", "00100", "11111", "00100", "00100", "00000"],
    "-": ["00000", "00000", "00000", "11111", "00000", "00000", "00000"],
    "=": ["00000", "11111", "00000", "11111", "00000", "00000", "00000"],
    ".": ["00000", "00000", "00000", "00000", "00000", "01100", "01100"],
    "/": ["00001", "00010", "00010", "00100", "01000", "01000", "10000"],
    "?": ["01110", "10001", "00001", "00010", "00100", "00000", "00100"],
}


MESSAGE_PRESETS = {
    "aim c7 flag": ["AIM C7", "FLAG"],
    "camera access needed": ["CAMERA", "ACCESS"],
    "check c7 flag": ["CHECK C7", "FLAG"],
    "check markers": ["CHECK", "MARKERS"],
    "move closer": ["MOVE", "CLOSER"],
    "move ear tag up": ["MOVE EAR", "TAG UP"],
    "move shoulder tag down": ["MOVE", "SHOULDER", "DOWN"],
    "no person found": ["NO PERSON", "FOUND"],
    "recheck ear and c7": ["RECHECK", "EAR + C7"],
    "restarting": ["RESTARTING"],
}


def is_usb_bottom(orientation):
    return orientation == USB_BOTTOM


def transform_point(x, y, orientation):
    if is_usb_bottom(orientation):
        return WIDTH - 1 - x, HEIGHT - 1 - y
    return x, y


def draw_line(x1, y1, x2, y2, orientation):
    x1, y1 = transform_point(x1, y1, orientation)
    x2, y2 = transform_point(x2, y2, orientation)
    display.line(x1, y1, x2, y2)


def draw_dashed_line(x1, y1, x2, y2, orientation, dash=9, gap=6):
    dx = x2 - x1
    dy = y2 - y1
    distance = (dx * dx + dy * dy) ** 0.5
    if distance <= 0:
        return
    cursor = 0
    while cursor < distance:
        end = min(cursor + dash, distance)
        start_ratio = cursor / distance
        end_ratio = end / distance
        draw_line(
            int(round(x1 + dx * start_ratio)),
            int(round(y1 + dy * start_ratio)),
            int(round(x1 + dx * end_ratio)),
            int(round(y1 + dy * end_ratio)),
            orientation,
        )
        cursor += dash + gap


def draw_rect(x, y, w, h, orientation):
    if is_usb_bottom(orientation):
        x = WIDTH - x - w
        y = HEIGHT - y - h
    display.rectangle(x, y, w, h)


def flip_curve_point(x, y):
    return WIDTH - 1 - x, y


def curve_points_for_display(points):
    return [flip_curve_point(x, y) for x, y in points]


def pixel_text_width(text, scale):
    width = 0
    for char in text:
        width += (4 if char == " " else 6) * scale
    return max(0, width - scale)


def draw_pixel_text(text, x, y, scale, orientation):
    cursor = x
    for char in text:
        if char == " ":
            cursor += 4 * scale
            continue
        glyph = PIXEL_FONT.get(char, PIXEL_FONT["?"])
        for row, cells in enumerate(glyph):
            for col, cell in enumerate(cells):
                if cell == "1":
                    draw_rect(cursor + col * scale, y + row * scale, scale, scale, orientation)
        cursor += 6 * scale


def draw_centered_text(text, y, scale, orientation):
    x = max(8, (WIDTH - pixel_text_width(text, scale)) // 2)
    draw_pixel_text(text, x, y, scale, orientation)


def draw_quality_strip(bits, orientation):
    bits = bits[-16:]
    if not bits:
        return
    x = WIDTH - 13
    y = 8
    for bit in bits:
        if bit == "1":
            draw_rect(x, y, 5, 5, orientation)
        else:
            draw_line(x, y + 2, x + 4, y + 2, orientation)
        y += 7


def draw_border(orientation):
    draw_line(0, 0, WIDTH - 1, 0, orientation)
    draw_line(0, HEIGHT - 1, WIDTH - 1, HEIGHT - 1, orientation)
    draw_line(0, 0, 0, HEIGHT - 1, orientation)
    draw_line(WIDTH - 1, 0, WIDTH - 1, HEIGHT - 1, orientation)
    draw_line(5, 5, WIDTH - 6, 5, orientation)
    draw_line(5, HEIGHT - 6, WIDTH - 6, HEIGHT - 6, orientation)
    draw_line(5, 5, 5, HEIGHT - 6, orientation)
    draw_line(WIDTH - 6, 5, WIDTH - 6, HEIGHT - 6, orientation)


def message_lines(message):
    cleaned = " ".join(message.strip().split())
    preset = MESSAGE_PRESETS.get(cleaned.lower())
    if preset:
        return preset

    words = cleaned.upper().split()
    if not words:
        return ["NO STATUS"]

    lines = []
    current = ""
    for word in words:
        next_line = word if not current else current + " " + word
        if len(next_line) <= 11:
            current = next_line
            continue
        if current:
            lines.append(current)
        current = word[:11]
        if len(lines) == 2:
            break
    if current and len(lines) < 3:
        lines.append(current)
    return lines[:3]


def draw_message(message, orientation):
    lines = message_lines(message)
    scale = 4 if len(lines) <= 2 else 3
    line_height = 7 * scale
    line_gap = 10 if len(lines) <= 2 else 7
    block_height = len(lines) * line_height + (len(lines) - 1) * line_gap
    top = max(10, (HEIGHT - block_height) // 2)

    clear()
    display.thickness(3)
    draw_border(orientation)
    display.thickness(2)
    for i, line in enumerate(lines):
        draw_centered_text(line, top + i * (line_height + line_gap), scale, orientation)
    display.update()


def draw_points(points, note="", orientation=USB_TOP, baseline_points=None, quality_bits=""):
    baseline_points = baseline_points or []
    curve_points = curve_points_for_display(points)
    baseline_curve_points = curve_points_for_display(baseline_points)
    clear()

    # Portrait mode: place the Badger on its short edge. The body chain uses
    # the 296px axis; forward/back drift uses the 128px axis.
    if len(baseline_curve_points) > 1:
        display.thickness(1)
        for i in range(len(baseline_curve_points) - 1):
            x1, y1 = baseline_curve_points[i]
            x2, y2 = baseline_curve_points[i + 1]
            draw_dashed_line(x1, y1, x2, y2, orientation)
        for x, y in baseline_curve_points:
            draw_rect(x - 2, y - 2, 5, 5, orientation)
    else:
        cy = HEIGHT // 2
        display.thickness(1)
        draw_line(18, cy, WIDTH - 18, cy, orientation)
        draw_line(18, cy - 14, 18, cy + 14, orientation)
        draw_line(WIDTH - 18, cy - 14, WIDTH - 18, cy + 14, orientation)

    if len(curve_points) > 1:
        display.thickness(4)
        for i in range(len(curve_points) - 1):
            x1, y1 = curve_points[i]
            x2, y2 = curve_points[i + 1]
            draw_line(x1, y1, x2, y2, orientation)

    display.thickness(1)
    for x, y in curve_points:
        draw_rect(x - 3, y - 3, 7, 7, orientation)

    if note:
        draw_pixel_text(note[:14].upper(), 8, 8, 2, orientation)

    display.thickness(1)
    draw_quality_strip(quality_bits, orientation)

    display.update()


def parse_payload(line):
    parts = line.strip().split(",")
    if not parts:
        return "", USB_TOP, None, None, "", ""
    if parts[0] == "M":
        orientation = USB_TOP
        message_start = 1
        if len(parts) > 1 and parts[1] in (USB_TOP, USB_BOTTOM):
            orientation = parts[1]
            message_start = 2
        return (
            "M",
            orientation,
            None,
            None,
            "",
            ",".join(parts[message_start:]).strip() or "No person found",
        )
    if len(parts) < 4 or parts[0] != "P":
        return "", USB_TOP, None, None, "", ""
    try:
        orientation = USB_TOP
        count_index = 1
        if parts[count_index] in (USB_TOP, USB_BOTTOM):
            orientation = parts[count_index]
            count_index += 1
        n = int(parts[count_index])
        coord_start = count_index + 1
        coords = parts[coord_start : coord_start + n * 2]
        points = []
        for i in range(0, len(coords), 2):
            x = max(0, min(WIDTH - 1, int(coords[i])))
            y = max(0, min(HEIGHT - 1, int(coords[i + 1])))
            points.append((x, y))
        note_index = coord_start + n * 2
        note = parts[note_index] if len(parts) > note_index else ""
        baseline_points = []
        quality_bits = ""
        index = note_index + 1
        while index < len(parts):
            marker = parts[index]
            if marker == "B" and index + 1 < len(parts):
                baseline_n = int(parts[index + 1])
                baseline_coord_start = index + 2
                baseline_coords = parts[baseline_coord_start : baseline_coord_start + baseline_n * 2]
                baseline_points = []
                for i in range(0, len(baseline_coords), 2):
                    x = max(0, min(WIDTH - 1, int(baseline_coords[i])))
                    y = max(0, min(HEIGHT - 1, int(baseline_coords[i + 1])))
                    baseline_points.append((x, y))
                index = baseline_coord_start + baseline_n * 2
            elif marker == "Q" and index + 1 < len(parts):
                quality_bits = "".join([bit for bit in parts[index + 1] if bit in ("0", "1")])
                index += 2
            else:
                break
        return "P", orientation, points, baseline_points, quality_bits, note
    except Exception:
        return "", USB_TOP, None, None, "", ""


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
    kind, orientation, points, baseline_points, quality_bits, note = parse_payload(line)
    if kind == "M":
        draw_message(note, orientation)
        ack("OK,M")
    elif kind == "P" and points:
        draw_points(points, note, orientation, baseline_points, quality_bits)
        ack("OK,P,{}".format(len(points)))
    else:
        ack("ERR,BAD_PAYLOAD")
