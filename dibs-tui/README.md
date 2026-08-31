# dibstop

A live view of the benchmark machine's lock, and a way to act on what is holding it.

It owns no state. `dibs --watch --json` is the feed, over the single persistent
connection that already exists, and every action shells back out to the same wrapper, so
nothing here can disagree with what `dibs --status` would have said. A redraw costs a lock
read on the far side rather than a fresh login, which is what makes it safe to leave open
beside a benchmark.

    cargo build --release && cp target/release/dibstop ~/.local/bin/

Run `dibstop [interval]`, default 2 seconds. `?` lists the keys.

`dibs` has to be on PATH; this is only a front end for it.

## Seeing what it draws

`pty-run.py` runs the TUI under a pseudo-terminal and prints the screen. ratatui redraws by
moving the cursor and rewriting only what changed, so the output is not a series of frames
that can be split apart; this applies the escapes to a grid instead, which is the only honest
way to read one.

    python3 pty-run.py "dibstop 2" ROWS COLS SECONDS [keys]

Keys are backslash-escaped and sent halfway through, so a wheel notch is
`'\x1b[<65;10;5M'` and a digit is just `5`. Every TUI change in this repo was checked this way.
