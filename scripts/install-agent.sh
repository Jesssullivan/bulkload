#!/usr/bin/env bash
# Install a built bulkload-agent so a Nix garbage collection cannot break it
# (M2 item 4, 2026-10-16: a GC deleted the glibc an installed agent was
# linked against). A binary built inside `nix develop` names /nix/store
# paths: its ELF interpreter and RUNPATH on Linux, its dylibs on Darwin.
# None of them is a GC root, so `nix-collect-garbage` may delete them and
# the binary then fails to start. This script copies the binary to DEST_DIR
# under NAME and adds one indirect GC root per store path it names, under
# ROOT_DIR (default ${XDG_STATE_HOME:-$HOME/.local/state}/bulkload/gcroots),
# writes NAME.sha256 and NAME.gcroots beside it, then runs its selftest,
# which reports whether its interpreter is present and rooted.
#
# Usage: install-agent.sh BINARY DEST_DIR NAME [ROOT_DIR]
# Never overwrites an existing NAME in DEST_DIR. R-N13.
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
mkdir -p "$dest_dir" "$root_dir"
target=$dest_dir/$name
[[ -e $target || -L $target ]] && { echo "install-agent: $target exists; not replaced" >&2; exit 3; }

# Every /nix/store path the binary names, as the top-level store entry.
mapfile -t store_paths < <(
    LC_ALL=C grep -aoE '/nix/store/[0-9a-df-np-sv-z]{32}-[^/[:space:][:cntrl:]:]+' "$binary" | sort -u
)
if [[ ${#store_paths[@]} -gt 0 ]] && ! command -v nix-store >/dev/null 2>&1; then
    echo "install-agent: $binary names ${#store_paths[@]} /nix/store paths and nix-store is not on PATH" >&2
    exit 4
fi

staging=$(mktemp "$dest_dir/.install-agent.XXXXXX")
trap 'rm -f "$staging"' EXIT
cp "$binary" "$staging"
chmod 0755 "$staging"

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
    manifest+=("$root -> $path")
done

# Publish without replacing anything that appeared meanwhile.
ln "$staging" "$target"
rm -f "$staging"
trap - EXIT
(cd "$dest_dir" && sha256sum "$name" > "$name.sha256")
printf '%s\n' "${manifest[@]}" > "$target.gcroots"
echo "install-agent: $target, ${#store_paths[@]} store paths rooted under $root_dir"
BULKLOAD_GCROOTS=$root_dir "$target" selftest
