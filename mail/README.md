# mail/ — the shape of a message in this mailbox

One file per message, `YYYY-MM-DD-<slug>.md`, committed. Four header lines, aligned on the colon, then
the body, then the effort at the end of the same file:

```
kind:    contract | report | direction | resource | cost
from:    <the sender's node name>
to:      <the receiver's node name>
spend:   idle-compute | tokens | person_attention | person_time | machine_time | money
id:      <what this answers or what it is about; a slug or a path>
```

- The body opens by naming the node whose roadmap the message changes, states the finding in the one
  sentence it exists for, then gives evidence pointers (paths, commands) and closes with **the
  decision needed** — `none` is a complete answer.
- The effort goes at the end: how long the sender held this, and what it cost. A message that costs
  the receiver something and carries no effort is bounced back with one line naming the gap.
- A message is read once. Land what it says in the document that owns the fact, then delete the file
  and commit in the same step; deletion is tidying, not a receipt and not a reply.
- This is one shared mailbox, not a per-node inbox: direction is set by the `to:` line, so a message
  to `root` and a message to `flashdb` sit side by side. The sender writes, the receiver lands it and
  deletes it; nobody deletes a message they did not receive.
