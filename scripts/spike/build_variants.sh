#!/usr/bin/env bash
# Build the release binary once, keep the LTO object, then relink it into
# layout variants:
#
#   shuf-NN  lld --shuffle-sections=*=NN          (NN = 1..16)
#   ctrl-NN  the default link, relinked again     (16 copies, should be one md5)
#   pad-NN   no shuffle, a pad of 16*k bytes before .text
#
# Writes variants/<name> and variants/manifest.tsv. Throwaway spike code.
set -euo pipefail

out=$(realpath -m "${1:-variants}")
nshuf=${NSHUF:-16}
nctrl=${NCTRL:-16}
npad=${NPAD:-8}
mkdir -p "$out"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target-spike}

echo "== toolchain"
rustc -vV
cc --version | head -1

echo "== plain release build (the reference md5)"
time cargo build --release --quiet
cp "$CARGO_TARGET_DIR/release/arche" "$out/plain"
readelf -p .comment "$out/plain" || true

echo "== release build again with save-temps and printed link args"
# a fresh target dir so the bin crate is really rebuilt with these flags
T2=$CARGO_TARGET_DIR-temps
time env CARGO_TARGET_DIR="$T2" \
    RUSTFLAGS="-C target-cpu=x86-64-v2 -C save-temps --print link-args" \
    cargo build --release -v >"$out/../build-temps.log" 2>&1
link=$(grep -E '^LC_ALL=' "$out/../build-temps.log" \
    | grep -E '"-o" "[^"]*/release/deps/arche-[0-9a-f]+"' | tail -1)
[ -n "$link" ] || { echo "no link line found"; tail -50 "$out/../build-temps.log"; exit 1; }
echo "$link" > "$out/../link-line.txt"
cmp "$T2/release/arche" "$out/plain" \
    && echo "save-temps build is byte-identical to the plain build" \
    || echo "NOTE: save-temps build differs from the plain build"

relink() { # relink <outfile> [extra quoted args...]
    local dst=$1; shift
    local extra=""
    for a in "$@"; do extra="$extra \"$a\""; done
    local cmd
    cmd=$(printf '%s' "$link" | sed -E "s#\"-o\" \"[^\"]*\"#\"-o\" \"$dst\"$extra#")
    bash -c "$cmd"
}

printf 'name\tkind\tseed\tpad\tmd5\n' > "$out/manifest.tsv"
start=$(date +%s.%N)
for i in $(seq 1 "$nctrl"); do
    n=$(printf 'ctrl-%02d' "$i")
    relink "$out/$n"
    printf '%s\tctrl\t0\t0\t%s\n' "$n" "$(md5sum < "$out/$n" | cut -c1-32)" >> "$out/manifest.tsv"
done
for i in $(seq 1 "$nshuf"); do
    n=$(printf 'shuf-%02d' "$i")
    relink "$out/$n" "-Wl,--shuffle-sections=*=$i"
    printf '%s\tshuf\t%d\t0\t%s\n' "$n" "$i" "$(md5sum < "$out/$n" | cut -c1-32)" >> "$out/manifest.tsv"
done
for i in $(seq 1 "$npad"); do
    n=$(printf 'pad-%02d' "$i")
    # deterministic pads spread over one page, in units of the .text alignment
    pad=$(( ( (i * 97) % 256 ) * 16 - 1 ))
    printf 'SECTIONS { .textpad : { BYTE(0); . += %d; } } INSERT BEFORE .text;\n' "$pad" > "$out/$n.ld"
    relink "$out/$n" "-Wl,-T,$out/$n.ld"
    printf '%s\tpad\t0\t%d\t%s\n' "$n" "$((pad + 1))" "$(md5sum < "$out/$n" | cut -c1-32)" >> "$out/manifest.tsv"
done
end=$(date +%s.%N)
total=$((nctrl + nshuf + npad))
echo "== $total relinks in $(awk "BEGIN{print $end - $start}") s"

echo "== manifest"
column -t "$out/manifest.tsv"
echo "distinct md5 by kind:"
tail -n +2 "$out/manifest.tsv" | awk '{print $2, $5}' | sort -u | awk '{print $1}' | uniq -c
echo "plain md5: $(md5sum < "$out/plain" | cut -c1-32)"
echo "== alpha_beta address per variant (first 4 of each kind)"
for f in "$out"/ctrl-0[1-4] "$out"/shuf-0[1-4] "$out"/pad-0[1-4]; do
    printf '%s ' "$(basename "$f")"
    { nm "$f" | grep 'AlphaBeta10alpha_beta' | head -1 | cut -d' ' -f1; } || true
done
