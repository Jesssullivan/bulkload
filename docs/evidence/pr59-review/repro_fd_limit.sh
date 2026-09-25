#!/usr/bin/env bash
# PR #59 review: W3 exhausts descriptors at the macOS default soft limit (256).
# Usage: repro_fd_limit.sh <bulkload-agent binary> <scratch dir>
set -u
B=$1; S=$2
rm -rf "$S"; mkdir -p "$S/src" "$S/dst"
python3 -c "
import os
for i in range(700):
    open('$S/src/f%04d'%i,'wb').write(os.urandom(2000))
"
ulimit -n 256
"$B" copy "$S/src" "$S/dst" "$S/ss" "$S/ds" >"$S/out.txt" 2>&1
echo "rc=$?"; head -1 "$S/out.txt"; tail -1 "$S/out.txt"; echo "published: $(ls "$S/dst" | wc -l) of 700"
# Observed 2026-09-23 on neo: PR head -> rc=1 "refused: IO (errno 24)" or 475 IO refusals;
# base c3f0b02 binary -> completed=700 refusals=0.
# Automated as tests/fd_limit.rs. Fixed on the W3 branch after 28976c7 (sized
# descriptor budget, shared parent fds, raised soft limit): completed=700.
