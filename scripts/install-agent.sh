#!/usr/bin/env bash
# Install a built bulkload-agent so a Nix garbage collection cannot break it
# (M2 item 4, 2026-10-16: a GC deleted the glibc an installed agent was
# linked against). A binary built inside `nix develop` names /nix/store
# paths: its ELF interpreter and RUNPATH on Linux, its dylibs on Darwin.
# None of them is a GC root, so `nix-collect-garbage` may delete them and
# the binary then fails to start. This script:
#   1. copies the binary to a temporary beside DEST_DIR/NAME; on Linux with
#      patchelf, drops RUNPATH entries outside /nix/store (the devShell adds
#      its own `outputs/out/lib`, which nothing should be able to plant
#      libraries in);
#   2. adds one indirect GC root per store path the copy names, under
#      ROOT_DIR (default ${XDG_STATE_HOME:-$HOME/.local/state}/bulkload/
#      gcroots), each registered by `nix-store --add-root`;
#   3. publishes NAME without replacing anything, then NAME.sha256 and
#      NAME.gcroots (never over existing ones), then runs its selftest,
#      which reports whether every store path it names is rooted.
# A failure before the publish removes the roots this run created. Roots
# outlive a deleted binary: remove ROOT_DIR/NAME-* with it (the next GC then
# drops their registrations).
#
# Usage: install-agent.sh BINARY DEST_DIR NAME [ROOT_DIR]
# Never overwrites NAME, NAME.sha256 or NAME.gcroots. R-N13.
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo "usage: $0 BINARY DEST_DIR NAME [ROOT_DIR]" >&2
    exit 2
fi
binary=$1
dest_dir=$2
name=$3
root_dir=${4:-${XDG_STATE_HOME:-$HOME/.local/state}/bulkload/gcroots}

case $name in
    */* | "" | . | ..) echo "install-agent: NAME must be a plain file name" >&2; exit 2 ;;
esac
[[ -f $binary && -x $binary ]] || { echo "install-agent: $binary is not an executable file" >&2; exit 2; }
if command -v sha256sum >/dev/null 2>&1; then
    digest() { sha256sum "$1"; }
elif command -v shasum >/dev/null 2>&1; then
    digest() { shasum -a 256 "$1"; }
else
    echo "install-agent: neither sha256sum nor shasum is on PATH" >&2
    exit 2
fi
mkdir -p "$dest_dir" "$root_dir"
target=$dest_dir/$name
for path in "$target" "$target.sha256" "$target.gcroots"; do
    if [[ -e $path || -L $path ]]; then
        echo "install-agent: $path exists; not replaced" >&2
        exit 3
    fi
done

staging=$(mktemp "$dest_dir/.install-agent.XXXXXX")
created=()
cleanup() {
    rm -f "$staging"
    if [[ ${#created[@]} -gt 0 ]]; then
        rm -f "${created[@]}"
    fi
}
trap cleanup EXIT
cp "$binary" "$staging"
chmod 0755 "$staging"

# A RUNPATH entry outside /nix/store (the devShell's `$out/lib`) is dropped
# where patchelf can; elsewhere it is reported.
if command -v readelf >/dev/null 2>&1 && LC_ALL=C readelf -h "$staging" >/dev/null 2>&1; then
    runpath=$(LC_ALL=C readelf -d "$staging" | sed -n 's/.*(RUNPATH).*\[\(.*\)\]/\1/p')
    if [[ -n $runpath ]]; then
        kept=$(tr ':' '\n' <<<"$runpath" | grep '^/nix/store/' | paste -sd: - || true)
        if [[ $kept != "$runpath" ]]; then
            if command -v patchelf >/dev/null 2>&1; then
                patchelf --set-rpath "$kept" "$staging"
                echo "install-agent: RUNPATH reduced to its /nix/store entries"
            else
                echo "install-agent: warning: RUNPATH names paths outside /nix/store: $runpath" >&2
            fi
        fi
    fi
fi

# Every /nix/store path the copy names, as the top-level store entry (the
# store's hash alphabet, then its name characters).
mapfile -t store_paths < <(
    LC_ALL=C grep -aoE '/nix/store/[0-9a-df-np-sv-z]{32}-[A-Za-z0-9+._?=-]+' "$staging" | sort -u
)
if [[ ${#store_paths[@]} -gt 0 ]] && ! command -v nix-store >/dev/null 2>&1; then
    echo "install-agent: $binary names ${#store_paths[@]} /nix/store paths and nix-store is not on PATH" >&2
    exit 4
fi
manifest=()
index=0
for path in "${store_paths[@]}"; do
    if [[ ! -e $path ]]; then
        echo "install-agent: $binary names $path, which is gone; rebuild it" >&2
        exit 5
    fi
    index=$((index + 1))
    root=$root_dir/$name-$index-${path#/nix/store/}
    nix-store --add-root "$root" --realise "$path" >/dev/null
    created+=("$root")
    manifest+=("$root -> $path")
done

# Publish without replacing anything that appeared meanwhile: link(2), so
# a file or directory at the name is an error, never a parent.
link "$staging" "$target"
rm -f "$staging"
created=()
trap - EXIT
set -o noclobber
(cd "$dest_dir" && digest "$name" > "$name.sha256")
printf '%s\n' "${manifest[@]}" > "$target.gcroots"
echo "install-agent: $target, ${#store_paths[@]} store paths rooted under $root_dir"
BULKLOAD_GCROOTS=$root_dir "$target" selftest
