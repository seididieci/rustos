#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

echo "[run] Scheduler: RT a 32 priorita' + CBS"

# Build dei binari userspace (userland/ = servizi utente) e della test suite
# (testland/) in pipeline separate, poi il kernel che li embedda entrambi
# (user_binary.rs via include_bytes!).
./scripts/build-userland.sh
./scripts/build-tests.sh

# Diagnostica scheduler/IRQ (feature `sched_debug`): snapshot ready-mask ogni
# 100 tick + log IRQ1. Spenta di default (log puliti); accesa con
# SCHED_DEBUG=1. run-tests.sh la imposta sempre.
if [ "${SCHED_DEBUG:-0}" = "1" ]; then
    echo "[run] sched_debug ATTIVO (log scheduler + IRQ1)"
    cargo build --release --features sched_debug
else
    cargo build --release
fi

# Immagini disco FAT32 (Fase 9.2 + 16d): generate a ogni run.
# fat.img = disco di boot (UUID 4F4C4556, label VELORDOR, montato a /fat);
# fat2.img = secondo disco (UUID e label diversi + MARKER.TXT) per i test di
# identità stabile (t36) e il reorder (SWAP_DRIVES=1 inverte l'ordine IDE:
# le lettere sdX si scambiano, UUID=/LABEL= restano validi).
python3 scripts/mkfat.py userland/fs/fat.img
python3 scripts/mkfat.py userland/fs/fat2.img --serial C0FFEE01 --label SECOND --marker "second disk marker"

# Servizi da disco (Fase 21): /bin + /test iniettati via script condiviso
# (stesso usato da test-shell.py) DOPO mkfat. Fail-loud.
bash scripts/inject-bins.sh

KERNEL=target/x86_64-unknown-none/release/velordor-kernel
DISPLAY="${RUN_DISPLAY:-none}"   # RUN_DISPLAY=gtk per vedere la VGA in locale

# Boot diretto via protocollo PVH (ELF64 + nota XEN_ELFNOTE_PHYS32_ENTRY):
# QEMU carica il kernel e trasferisce il controllo in protected mode 32-bit.
# Due drive IDE (Fase 16d): userdisk li enumera sda,sdb in ordine di probe
# e userfs monta a /fat per UUID (mai per lettera).
if [ "${SWAP_DRIVES:-0}" = "1" ]; then
    DRIVES="-drive file=userland/fs/fat2.img,format=raw,if=ide -drive file=userland/fs/fat.img,format=raw,if=ide"
else
    DRIVES="-drive file=userland/fs/fat.img,format=raw,if=ide -drive file=userland/fs/fat2.img,format=raw,if=ide"
fi
# shellcheck disable=SC2086
exec qemu-system-x86_64 \
    -m 256M \
    -display "$DISPLAY" \
    -serial stdio \
    -no-reboot \
    -kernel "$KERNEL" \
    $DRIVES \
    "$@"
