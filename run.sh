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

# Immagine disco FAT32 per il fs server (Fase 9.2): generata a ogni run.
python3 scripts/mkfat.py userland/fs/fat.img

KERNEL=target/x86_64-unknown-none/release/velordor-kernel
DISPLAY="${RUN_DISPLAY:-none}"   # RUN_DISPLAY=gtk per vedere la VGA in locale

# Boot diretto via protocollo PVH (ELF64 + nota XEN_ELFNOTE_PHYS32_ENTRY):
# QEMU carica il kernel e trasferisce il controllo in protected mode 32-bit.
# Il drive IDE monta il FAT32 che userdisk legge via ATA PIO (Fase 16) e userfs
# monta a /fat via IPC (porte ATA solo a userdisk).
exec qemu-system-x86_64 \
    -m 256M \
    -display "$DISPLAY" \
    -serial stdio \
    -no-reboot \
    -kernel "$KERNEL" \
    -drive file=userland/fs/fat.img,format=raw,if=ide \
    "$@"
