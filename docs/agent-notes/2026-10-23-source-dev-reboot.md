# 2026-10-23 — estate pass 5 refused its own carry outputs after a neo reboot

SPEC rules served: 7 (never clobber: nothing anyone else wrote is replaced)
and 8 (idempotent: a rerun converges and reads only what changed).

## What happened

Pass 5 of `estate-20261022` ran on agent 90acd4f. Its carry pull refused 981
of 988 seats `DESTINATION_OCCUPIED`, with `outputs_superseded=0` and
`sqlite_refused_output_commits=981`. Pass 4 had superseded its own 962
earlier outputs.

Between the two passes, neo ran out of battery and rebooted. macOS gives an
APFS volume a new `st_dev` at boot. The source root's device went from
16777234 to 16777232; its inode, 606191176, did not change.

The destination ledger is
`~/.local/state/bulkload/move/estate-20261022/pull/plan/transfer.sqlite`.
Read-only, it shows:

| Table | Rows | Source root device |
| --- | --- | --- |
| `outputs` | 987 | 16777234 |
| `outputs` | 7 | 16777232 |
| `owned_outputs` | 2 | 16777234 |
| `refused_outputs` | 981 | 16777232 |

Everything else in the authority is the same for all of them: store
authority `9bdf844a…`, source path, source inode, and the destination root
with its device and inode. `homes-20261021/pull/plan` holds 6 rows under
each device.

## Cause

Every destination row key starts with the session's output authority. That
authority includes the source authority, which is the source store id, the
root path and the root's `(dev, ino)`. The seat's own `dev` is in the row
part of the key.

When the device number changed, the authority changed with it. The range
read that finds a path's own outputs (`Store::output_rows`) then returned
nothing. A regenerated seat therefore met a file with no owning row, and it
was refused instead of superseding bulkload's own output.

## Fix (branch fix/own-output-source-dev-reboot-20261023)

- `Store::output_authorities` lists each authority the store's rows are
  keyed under. It reads `outputs`, `owned_outputs` and `consolidated`, and
  costs one seek per distinct authority.
- `transfer::source_dev::DevAliases` keeps the earlier authorities that
  match the session's own in everything except the source root's device.
  It does this by decoding the authority as `serve` and `receive` compose
  it.
- Ownership: `owned_output` reads rows under the session's authority and
  under the earlier ones. The file at the path must still equal one recorded
  identity exactly, so that check is no looser.
- Remembered refusals: an `Occupied` or `NotPrefix` refusal no longer stands
  when the file is exactly one this store's rows record at the path. This
  is the live state: pass 5's 981 `refused_outputs` rows, keyed under the
  new device.
- Reuse: an unchanged seat matches its row under the earlier key. A seat on
  the root's device is looked up with the root's earlier device. The same
  applies to consolidation records. The source reads nothing for these
  seats.
- Counter `transfer_source_dev_aliases` counts the earlier authorities each
  session found.

## Ledgers checked

- **Source capture ledger (`captures`) and `refused_seats`.** These are keyed
  under the source authority too, so they miss after a reboot. Once the
  destination answers reuse, the source never consults them for an
  unchanged seat. A changed seat is read anyway. Not changed.
- **`directories` pending records.** These are keyed `(authority, path)`. A
  record a crash left behind is missed after a source reboot. The
  directory's mode is then kept and reported (`dest_directory_mode_kept`),
  not refused. Not changed.
- **Unrowed capture records (xattr `record_key`) and `SQLite` snapshot
  keys.** These carry the seat's `dev`. After a reboot they fall back to the
  manifest path, which costs one source read and no refusal. Not changed.

## Validation

Validation ran on honey (`~/git-build/srcdev-20261023`, `nice -n 19`): the
targeted tests, `cargo fmt --check`, both clippy passes and gitleaks. The
results are in the PR.
