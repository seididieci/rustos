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
# Oltre agli hash emette `build-meta/service_policy.rs` (Fase 45, sandbox
# build): tabella `SERVICE_POLICY: &[(hash, ops_mask)]` con la mask dei
# diritti FS consentiti a CIASCUN servizio noto (userfs la applica come
# tetto via `peer_info`, vedi `userland/fs/src/policy.rs`). I servizi TCB
# hanno ALL (fiducia per parentela+hash, Strato 1+2); i programmi di terzi
# hanno righe restrittive esplicite (sotto: solo runhello oggi — il futuro
# toolchain avra' le sue righe, zero redesign). Gli hash ignoti al manifest
# cadono nel default restrittivo di userfs (niente MOUNT/UMOUNT/GRANT/PIPE).
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
POL_OUT="$OUT_DIR/service_policy.rs"
poltmp="$POL_OUT.tmp"

python3 - "$BUILD" "$tmp" "$poltmp" <<'EOF'
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
# userinit.bin e userfs.bin ESCLUSI: entrambi includono il manifest a compile
# time (init: expected_hash; userfs: driver_name_of), quindi il loro hash nel
# manifest sarebbe stale-by-construction E instabile (ciclo: il binario
# incorpora l'hash di se stesso → ogni build lo cambia → la successiva lo
# ricambia, mai fixpoint — osservato: HASH_USERFS flippa a ogni run).
# Non servono: init non verifica se stesso (impossibile per costruzione, lo
# misura il kernel) e non controlla fs (embedded, TCB); userfs non pinna se
# stesso (la regola same-image confronta due peer vivi, niente manifest).
# Il manifest copre esattamente i servizi caricati da disco + disk (embedded
# ma senza ciclo: userdisk non include il manifest) — per questi il fixpoint
# e' raggiunto in UN passaggio (i loro binari non incorporano alcun hash).
bins = [p for p in bins if os.path.basename(p) not in ("userinit.bin", "userfs.bin")]

lines = [
    "// Generato da scripts/gen-service-hashes.sh — MAI modificare a mano.",
    "// Identita' misurata (Fase 36, Strato 2 di ADR-0026): FNV-1a a 64 bit",
    "// (`syscall_numbers::image_hash`) sui binari userland (esclusi",
    "// userinit/userfs: incorporano il manifest, il loro hash sarebbe un",
    "// ciclo instabile — vedi filtro sotto).",
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

# Mask per-binario (Fase 45): nome stem -> mask ops (bit RIGHTS_* di
# syscall-numbers: OPEN=0x1 READ=0x2 WRITE=0x4 READDIR=0x8 MKDIR=0x10
# MOUNT=0x20 UMOUNT=0x40 DELETE=0x80 SEEK=0x100 GRANT=0x200 PIPE=0x400,
# ALL=0x7FF). Default ALL (servizi TCB); programmi di terzi restrittivi.
# NOTA: `run`+redirect scrive su fd concessi (il check WRITE scatta sul
# canale del FIGLIO) e legge stdin ridiretta: runhello ha bisogno di
# OPEN+READ+WRITE+READDIR (0x0F). Senza WRITE `run ./x > /o` si rompe (Fase
# 40.4). Resta negato: MKDIR/DELETE/SEEK/MOUNT/UMOUNT/GRANT/PIPE.
POLICY = {
    "userrunhello": 0x00F,  # OPEN|READ|WRITE|READDIR (programma di terzi)
}
DEFAULT_MASK = 0x7FF  # ALL (servizi TCB)

pol = [
    "// Generato da scripts/gen-service-hashes.sh — MAI modificare a mano.",
    "// Sandbox build (Fase 45): tetto ops per hash noto, applicato da userfs",
    "// (`userland/fs/src/policy.rs`) come `drop_mask & policy_mask`. Le mask",
    "// usano i bit RIGHTS_* di syscall-numbers (ALL=0x7FF con GRANT+PIPE).",
    "// Consumato via `include!(env!(\"VELORDOR_SERVICE_POLICY\"))` SOLO da",
    "// userfs (dopo service_hashes: referenzia le HASH_*). Rigenerato a build.",
    "pub const SERVICE_POLICY: &[(u64, u32)] = &[",
]
for p in bins:
    stem = os.path.splitext(os.path.basename(p))[0]
    const = "HASH_" + "".join(c.upper() if (c.isalnum()) else "_" for c in stem)
    mask = POLICY.get(stem, DEFAULT_MASK)
    tag = "terzi" if stem in POLICY else "TCB"
    pol.append("    (%s, 0x%03X), // %s" % (const, mask, tag))
pol.append("];")

with open(tmp, "w") as f:
    f.write("\n".join(lines) + "\n")
print("[gen-hashes] %d binari -> %s" % (len(bins), tmp))

poltmp = sys.argv[3] if len(sys.argv) > 3 else None
if poltmp:
    with open(poltmp, "w") as f:
        f.write("\n".join(pol) + "\n")
    print("[gen-hashes] policy %d righe -> %s" % (len(pol) - 6, poltmp))
EOF

mv "$tmp" "$OUT"
echo "[gen-hashes] scritto $OUT"
mv "$poltmp" "$POL_OUT"
echo "[gen-hashes] scritta $POL_OUT"
