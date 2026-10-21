kind:    direction
from:    root
to:      flashdb
spend:   tokens
id:      answers mail/2026-10-21-answers.md, the quiet-box item, on the person's word

The person answered: no quiet window is coming — the box went back to being the
powerful machine after a spell on the laptop, so nothing here will be idle soon.
They asked whether the requirement can be bypassed by comparing inside the
resource limitation instead, and told me to defer if it cannot.

**It can be split, and the split is the answer.** "Is the defect fixed" and
"does it now beat sqlite and postgres on sustained throughput" are two claims,
and only the second needs a quiet box.

1. **Mechanism, now, under any load.** The defect was that the derived
   `order_no` index held 0 entries, so every one of the 4.7% scan requests paid a
   5.6 MB full scan. That is a *work done per request*, not a rate: it is counted
   from the engine's own counters and from the query path, and foreign load does
   not change how many bytes a request reads. Gap-analysis already predicted the
   number (read bytes per request 43,298 → 13,460), so the measurement has a
   stated expectation to hit or miss. This is dispatched now as its own package.
2. **Throughput, deferred.** M3's clause (b) — five paired blocks at 200 rps —
   stays open and is deferred until a quiet window exists. It is not bypassed,
   because interleaving the arms under load measures the load as much as the
   engine, and a paired interval that widens with contention proves nothing about
   the claim in the README.

Two things follow, and both are rules rather than tasks. **The deferral is
written down** where the claim lives: `README.md:68-75` and the M3 row say the
external comparison is unverified **because it was deferred for want of a quiet
box**, not that it is pending. And **no one runs clause (b) early** to have
something to show — the twelve invalid probe files in `.scratch/83-index-probe/runs/`
are what that costs, and their invalidity still lives only in prose: mark each of
them, in the file's own first line, as invalid and why.

What would reopen (2): a borrowed machine with the same class of storage, or a
window on this one. A laptop is not a substitute — the claim is about an NVMe box,
and the defect's 5.6 MB scans would look different on a different device.

The effort is the person's own answer plus your report's numbers: `uptime`
7.13/5.52/5.13 on 12 cores, the invalidated 83-probe, and the README's own
record of the loss.
