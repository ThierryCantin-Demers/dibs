# The handoff report

A written-up account of the lock: the design, the findings, and everything another team
would have to change to run it on their own machines. Build it, then publish the HTML
wherever you like.

    python3 build-report.py

It reads `bin/dibs` and both test suites out of this repo, plus the ssh hook if you keep one,
and embeds them, so the appendix cannot drift from what actually runs. Rebuild after changing
any of them.
