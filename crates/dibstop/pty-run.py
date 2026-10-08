"""Run a TUI under a pty and print what is actually on the screen.

ratatui redraws by moving the cursor and rewriting only what changed, so the output is not
a sequence of frames that can be split apart. The only honest way to see a frame is to
apply the escapes to a grid, which is what this does: enough of one to place text.

    python3 pty-run.py "dibstop 2" ROWS COLS SECONDS [keys]
"""
import os, pty, fcntl, termios, struct, subprocess, select, time, sys, re

cmd, rows, cols, secs = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
import codecs
keys = codecs.decode(sys.argv[5], "unicode_escape") if len(sys.argv) > 5 else ""

m, s = pty.openpty()
fcntl.ioctl(s, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
p = subprocess.Popen(cmd.split(), stdin=s, stdout=s, stderr=s, close_fds=True)
os.close(s)

buf = b""
end = time.time() + secs
sent = not keys
while time.time() < end:
    r, _, _ = select.select([m], [], [], 0.2)
    if r:
        try:
            buf += os.read(m, 65536)
        except OSError:
            break
    if not sent and time.time() > end - secs / 2:
        os.write(m, keys.encode())
        sent = True
os.write(m, b"q")
time.sleep(0.4)
p.terminate()
p.wait()

grid = [[" "] * cols for _ in range(rows)]
cr = cc = 0
data = buf.decode("utf-8", "replace")
i = 0
while i < len(data):
    ch = data[i]
    if ch == "\x1b":
        mt = re.match(r"\x1b\[[?]?([0-9;]*)([A-Za-z])", data[i:])
        if not mt:
            i += 2
            continue
        args, verb = mt.group(1), mt.group(2)
        n = [int(x) if x else 0 for x in args.split(";")] if args else []
        if verb == "H":
            cr = (n[0] - 1) if len(n) > 0 and n[0] else 0
            cc = (n[1] - 1) if len(n) > 1 and n[1] else 0
        elif verb == "J" and (not n or n[0] == 2):
            grid = [[" "] * cols for _ in range(rows)]
        elif verb == "K":
            for x in range(cc, cols):
                grid[cr][x] = " "
        elif verb == "A":
            cr = max(0, cr - max(1, n[0] if n else 1))
        elif verb == "B":
            cr = min(rows - 1, cr + max(1, n[0] if n else 1))
        elif verb == "C":
            cc = min(cols - 1, cc + max(1, n[0] if n else 1))
        elif verb == "D":
            cc = max(0, cc - max(1, n[0] if n else 1))
        i += mt.end()
        continue
    if ch == "\n":
        cr = min(rows - 1, cr + 1)
        cc = 0
    elif ch == "\r":
        cc = 0
    elif ch >= " ":
        if 0 <= cr < rows and 0 <= cc < cols:
            grid[cr][cc] = ch
        cc += 1
        if cc >= cols:
            cc = 0
            cr = min(rows - 1, cr + 1)
    i += 1

print("\n".join("".join(row).rstrip() for row in grid).rstrip())
