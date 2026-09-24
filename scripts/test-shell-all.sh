#!/usr/bin/env bash
# Runner dei test interattivi di shell (sostituisce il monolite test-shell.py).
#
# Uso:
#   ./scripts/test-shell-all.sh            # sequenziale, immagini condivise
#   ./scripts/test-shell-all.sh --jobs 3   # parallelo, overlay qcow2/istanza
#   ./scripts/test-shell-all.sh --jobs 3 base 42   # solo fasi scelte
#   ./scripts/test-shell-all.sh source     # gate veloce: solo `source`
#                                          # (1 boot, ~10 s: per iterare)
#
# Parallelo: ogni istanza scrive su un overlay qcow2 privato (backing = le
# immagini generate una volta sola) — due QEMU sullo stesso raw read-write
# si corromperebbero (o il lock immagine blocca la seconda istanza).
# Sequenziale: si usano i raw direttamente (percorso storico, zero overhead).
set -u
cd "$(dirname "$0")/.."

JOBS=1
PHASES=()
while [ $# -gt 0 ]; do
    case "$1" in
        --jobs) JOBS="$2"; shift 2;;
        base|run|redirect|41|42|source|43|43b) PHASES+=("$1"); shift;;
        *) echo "fase ignota: $1 (base|run|redirect|41|42|source|43|43b)"; exit 2;;
    esac
done
[ ${#PHASES[@]} -eq 0 ] && PHASES=(base run redirect 41 42 source 43 43b)

echo "[all] preparo immagini FAT (una volta sola)"
python3 scripts/mkfat.py userland/fs/fat.img
python3 scripts/mkfat.py userland/fs/fat2.img --serial C0FFEE01 \
    --label SECOND --marker "second disk marker"
bash scripts/inject-bins.sh

declare -A PID_OF
FAILED=()
PASS=()

run_one() { # <fase> [overlay_fat overlay_fat2]
    local p="$1" ov1="${2:-}" ov2="${3:-}"
    local log="/tmp/test-shell-$p.log"
    local cmd=(python3 "scripts/test-shell-$p.py" --no-prep)
    if [ -n "$ov1" ]; then
        cmd+=(--fat "$ov1" --fat2 "$ov2" --fat-format qcow2)
    fi
    echo "[all] fase $p..."
    "${cmd[@]}" > "$log" 2>&1
    local rc=$?
    echo "[all] fase $p: exit=$rc (log $log)"
    return $rc
}

if [ "$JOBS" -le 1 ]; then
    for p in "${PHASES[@]}"; do
        if run_one "$p"; then PASS+=("$p"); else FAILED+=("$p"); fi
    done
else
    # Overlay privati per istanza (backing assoluto: qcow2 lo registra).
    # `-F raw`: qemu-img >= 10 rifiuta il backing senza formato (errore
    # fatale, niente overlay -> QEMU "No such file").
    for p in "${PHASES[@]}"; do
        qemu-img create -f qcow2 -F raw -b "$PWD/userland/fs/fat.img" \
            "/tmp/velordor-$p-fat.qcow2" > /dev/null
        qemu-img create -f qcow2 -F raw -b "$PWD/userland/fs/fat2.img" \
            "/tmp/velordor-$p-fat2.qcow2" > /dev/null
        run_one "$p" "/tmp/velordor-$p-fat.qcow2" "/tmp/velordor-$p-fat2.qcow2" &
        PID_OF[$p]=$!
    done
    for p in "${PHASES[@]}"; do
        if wait "${PID_OF[$p]}"; then PASS+=("$p"); else FAILED+=("$p"); fi
        rm -f "/tmp/velordor-$p-fat.qcow2" "/tmp/velordor-$p-fat2.qcow2"
    done
fi

echo "[all] PASS: ${PASS[*]:-nessuna}"
echo "[all] FAIL: ${FAILED[*]:-nessuna}"
[ ${#FAILED[@]} -eq 0 ]
