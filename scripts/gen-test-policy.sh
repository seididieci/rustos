#!/usr/bin/env bash
# Tabella policy per i binari testland (Fase 45, sandbox build).
#
# Genera `build-meta/test_policy.rs` con `pub const TEST_POLICY: &[(u64,u32)]`
# (hash FNV-1a -> mask ops, TUTTI ALL: la suite esercita ogni op). Incluso
# SOLO da userfs via `VELORDOR_TEST_POLICY` (mai dai binari test: niente ciclo
# hash-di-se', stesso motivo dell'esclusione userinit/userfs dal manifest
# servizi — vedi gen-service-hashes.sh).
#
# Esclusi:
# - `userforeign.bin` (l'attore "ignoto" di t57: DEVE restare fuori da ogni
#   tabella per provare il default restrittivo fail-closed);
# - `userfs.bin` (artefatto del rebuild di userfs in coda a build-tests.sh:
#   userfs e' fuori da entrambe le tabelle per costruzione, come userinit).
#
# Chiamato in coda a build-tests.sh (i .bin test esistono solo allora) e
# seguito dal rebuild di userfs (unico consumatore). Fixpoint in un passaggio:
# userfs e' escluso da entrambe le tabelle, i test non includono questa.
set -euo pipefail
cd "$(dirname "$0")/.."

BUILD="testland/build"
OUT_DIR="build-meta"
OUT="$OUT_DIR/test_policy.rs"

if [ ! -d "$BUILD" ]; then
    echo "[gen-test-policy] ERROR: $BUILD mancante (build-tests.sh prima)" >&2
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
# L'attore "ignoto" di t57 resta fuori da OGNI tabella (prova il default);
# userfs.bin (rebuild artefatto, vedi sopra) mai in tabella.
bins = [p for p in bins if os.path.basename(p) not in ("userforeign.bin", "userfs.bin")]

lines = [
    "// Generato da scripts/gen-test-policy.sh — MAI modificare a mano.",
    "// Sandbox build (Fase 45): hash dei binari testland -> mask ALL (la",
    "// suite esercita ogni op; i negativi stanno in t57 sul default ignoto).",
    "// Consumato via `include!(env!(\"VELORDOR_TEST_POLICY\"))` SOLO da userfs",
    "// (dopo service_hashes+service_policy). Rigenerato a ogni build test.",
    "pub const TEST_POLICY: &[(u64, u32)] = &[",
]
for p in bins:
    with open(p, "rb") as f:
        data = f.read()
    if not data:
        sys.exit("binario vuoto: %s" % p)
    lines.append("    (0x%016X, 0x7FF), // %s" % (fnv1a(data), os.path.basename(p)))
lines.append("];")

with open(tmp, "w") as f:
    f.write("\n".join(lines) + "\n")
print("[gen-test-policy] %d binari -> %s" % (len(bins), tmp))
EOF

mv "$tmp" "$OUT"
echo "[gen-test-policy] scritto $OUT"
