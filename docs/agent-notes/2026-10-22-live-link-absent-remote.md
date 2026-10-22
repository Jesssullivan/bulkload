# 2026-10-22: a live link with no source, over a transport

## Finding

This came from the `homes-pass-20261022` rerun (e1d3905, run on sting).

- The neo serve for each stage-link directory neo lacks refuses
  `SOURCE_ROOT_MISSING` before it sends any frame. Its stderr carries 821 of
  those lines.
- The local pull can only read that as end of stream, so it records and
  returns `TRANSPORT_LOST`.
- `pull_links` accepted only `SOURCE_ROOT_MISSING`, which local `copy`
  returns. Over ssh the claude-links step is therefore refused, and the
  re-seat is held.
- On 2026-10-21 the same refusal was there too, hidden behind the key-map IO.
  That run left 1004 link records with TRANSPORT_LOST sessions.
- The tests ran only locally (host `-`), so they never saw it.

## Fix (branch fix/live-link-absent-remote-20261022)

- `transfer::take_skip_seen` returns the `set_skip` prefixes the source walk
  offered an entry at or under.
- After a home's walk completes (Ok or ITEMS_REFUSED), a link it never
  reached has no source. No session runs for it; it prints
  `source=absent (nothing to merge)`, and any pull directory an earlier pass
  left for it is removed.
- Test: `homes_links_over_a_transport_skip_a_link_the_source_lacks` runs the
  homes-links case over an ssh stand-in. It fails on e1d3905, at the plan's
  TRANSPORT_LOST, and passes with the fix.
