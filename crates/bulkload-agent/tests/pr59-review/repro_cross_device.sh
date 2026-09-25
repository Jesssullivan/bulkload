#!/usr/bin/env bash
# PR #59 review: with the destination on a different device from its state
# store, a group-mode copy succeeds without one F_FULLFSYNC on the destination
# device. Usage: repro_cross_device.sh <bulkload-agent> <scratch dir>
# Needs INTERPOSE_DIR/libinterpose.dylib (cc -dynamiclib -o libinterpose.dylib interpose.c).
set -u
B=$1; S=$2; L=${INTERPOSE_DIR:?build interpose.c into a directory and set INTERPOSE_DIR}
mkdir -p "$S/src/sub" "$S/mnt"
head -c 3000000 /dev/urandom >"$S/src/a"; head -c 700000 /dev/urandom >"$S/src/sub/b"; printf hi >"$S/src/c"
hdiutil create -size 64m -fs APFS -volname bl59 -o "$S/dest.dmg" >/dev/null
hdiutil attach -nobrowse -mountpoint "$S/mnt" "$S/dest.dmg" >/dev/null
mkdir "$S/mnt/dest"
DYLD_INSERT_LIBRARIES="$L/libinterpose.dylib" FLUSHLOG="$S/flush.log" "$B" copy "$S/src" "$S/mnt/dest" "$S/sstate" "$S/dstate" | head -1
dest_dev=$(python3 -c "import os;print(os.stat('$S/mnt/dest').st_dev)")
echo "destination dev=$dest_dev"
echo "F_FULLFSYNC on destination device: $(grep -c "^F_FULLFSYNC dev=$dest_dev " "$S/flush.log")"
echo "F_BARRIERFSYNC on destination device: $(grep -c "^F_BARRIERFSYNC dev=$dest_dev " "$S/flush.log")"
hdiutil detach "$S/mnt" >/dev/null
# Observed on neo, PR head ab8e1d6: copy completed=3, F_FULLFSYNC on destination device: 0.
# Automated as tests/cross_device.rs (macOS, --ignored). Fixed on the W3 branch
# after 28976c7: F_FULLFSYNC on destination device: 3.
