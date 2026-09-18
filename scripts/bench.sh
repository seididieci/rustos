#!/usr/bin/env bash
# Bench throughput client→block su KVM (Fase 23).
#
# Esegue N boot con userbench (RUN_BENCH=1: init lo spawna prima della shell,
# senza suite) e raccoglie le righe `[bench]`. QEMU gira per sempre dopo il
# bench (shell): ogni run e' chiuso da `timeout` (exit 124 = atteso).
# Piattaforma di riferimento KVM (`-accel kvm -cpu host`); senza /dev/kvm
# ripiega su TCG con warning (tempi NON di riferimento).
set -euo pipefail
cd "$(dirname "$0")/.."

RUNS="${RUNS:-3}"
# 300 s: la prima run paga la build fredda (userland+testland+kernel, ~1-2 min
# su host lenti) + il boot; le successive sono incrementali. Con 120 s la prima
# run moriva in build (osservato: log troncato a userkbd) e il bench risultava
# "incompleto" senza aver mai avviato QEMU.
TIMEOUT_S="${TIMEOUT_S:-300}"

ACCEL=()
if [ -e /dev/kvm ]; then
    ACCEL=(-accel kvm -cpu host)
    echo "[bench] piattaforma: KVM (riferimento)"
else
    echo "[bench] WARNING: /dev/kvm assente, TCG (tempi non di riferimento)" >&2
fi

# shellcheck disable=SC2086
for i in $(seq 1 "$RUNS"); do
    log="/tmp/bench-run$i.log"
    echo "[bench] run $i/$RUNS (log $log)"
    code=0
    RUN_BENCH=1 timeout "$TIMEOUT_S" ./run.sh "${ACCEL[@]}" >"$log" 2>&1 || code=$?
    if [ "$code" -ne 124 ]; then
        echo "[bench] run $i FAILED (exit $code, atteso 124 da timeout)" >&2
        tail -20 "$log" >&2
        exit 1
    fi
    if ! grep -q '\[bench\] DONE ok=1' "$log"; then
        echo "[bench] run $i FAILED (bench incompleto)" >&2
        grep '\[bench\]' "$log" >&2 || tail -20 "$log" >&2
        exit 1
    fi
    grep '\[bench\]' "$log" | sed "s/^/[run$i] /"
done
