#!/usr/bin/env bash
# Genera il manifest degli hash dei servizi (Fase 36, Strato 2 di ADR-0026).
#
# Calcola FNV-1a a 64 bit (stesso algoritmo di `syscall_numbers::image_hash`,
# single source dell'identita' misurata) sui `.bin` finali di userland/build
# e scrive `build-meta/service_hashes.rs` con una `pub const HASH_*` per
# binario. I crate che ne hanno bisogno (init per il manifest, userfs per la
# policy `FS_REGISTER`, usertests per t51) lo includono con
# `include!(env!("VELORDOR_SERVICE_HASHES"))` — variabile esportata da
# build-userland.sh / build-tests.sh DOPO questa generazione.
#
# Correttezza dei byte misurati: gli stessi file vengono (a) embeddati nel
# kernel via `include_bytes!` (init/disk/fs) e (b) copiati verbatim su /fat
# via `inject-bins.sh` (mcopy non tocca il contenuto): l'hash qui calcolato e'
# quello dei byte effettivamente caricati dal loader ELF in entrambi i path.
#
# Fail-loud (set -euo + check espliciti): binari mancanti/vuoti o output non
# scrivibile = build interrotta, mai manifest stale silenzioso. Lo script
# rigenera SEMPRE da zero (niente append): rieseguire e' idempotente.
set -euo pipefail
cd "$(dirname "$0")/.."

BUILD="userland/build"
OUT_DIR="build-meta"
OUT="$OUT_DIR/service_hashes.rs"

if [ ! -d "$BUILD" ]; then
    echo "[gen-hashes] ERROR: $BUILD mancante (build-userland.sh prima)" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"
tmp="$OUT.tmp"

python3 - "$BUILD" "$tmp" <<'EOF'
import glob, os, sys

def fnv1a(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h

build, tmp = sys.argv[1], sys.argv[2]
bins = sorted(glob.glob(os.path.join(build, "*.bin")))
if not bins:
    sys.exit("nessun .bin in %s" % build)
# userinit.bin ESCLUSO: init e' compilato DOPO la generazione (ha bisogno del
# manifest) quindi il suo hash sarebbe stale-by-construction. Non serve: init
# non verifica se stesso (impossibile per costruzione) — lo misura il kernel
# allo spawn (`image_hash` nel PCB) e init e' TCB come il kernel che lo embedda.
bins = [p for p in bins if os.path.basename(p) != "userinit.bin"]

lines = [
    "// Generato da scripts/gen-service-hashes.sh — MAI modificare a mano.",
    "// Identita' misurata (Fase 36, Strato 2 di ADR-0026): FNV-1a a 64 bit",
    "// (`syscall_numbers::image_hash`) sui byte di ogni binario userland.",
    "// Consumatori via `include!(env!(\"VELORDOR_SERVICE_HASHES\"))`: init",
    "// (manifest pre-spawn), userfs (policy FS_REGISTER su identita'),",
    "// usertests (t51: peer_info atteso). Rigenerato a ogni build.",
]
for p in bins:
    with open(p, "rb") as f:
        data = f.read()
    if not data:
        sys.exit("binario vuoto: %s" % p)
    stem = os.path.splitext(os.path.basename(p))[0]
    const = "HASH_" + "".join(c.upper() if (c.isalnum()) else "_" for c in stem)
    lines.append("pub const %s: u64 = 0x%016X; // %s (%d B)" % (const, fnv1a(data), os.path.basename(p), len(data)))

with open(tmp, "w") as f:
    f.write("\n".join(lines) + "\n")
print("[gen-hashes] %d binari -> %s" % (len(bins), tmp))
EOF

mv "$tmp" "$OUT"
echo "[gen-hashes] scritto $OUT"
