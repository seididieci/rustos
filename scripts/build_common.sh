#!/usr/bin/env bash
# Funzioni condivise per la build freestanding dei crate utente/test.
#
# Usato SOLO via `source` da scripts/build-userland.sh e scripts/build-tests.sh,
# che si posizionano in CWD=radice del repo prima di chiamare `build_one`.
#
# Per ogni binario:
#   - CWD=radice: il config discovery di Cargo risale e trova la root .cargo
#     (build-std). Le rustflags user sono impostate qui via
#     CARGO_TARGET_*_RUSTFLAGS (unica fonte), NON nei file .cargo dei crate.
#   - Ogni binario ha il proprio linker script (-T) che forza caricamento a
#     USER_CODE e KEEP(.text._start).
#
# Le variabili `BUILD` e `CARGO_TARGET_DIR` devono essere esportate dal
# chiamante (es. userland/build e testland/build).

build_one() {
    local crate="$1"            # path del crate, es. userland/console
    local ld="$2"               # linker script del binario
    local out_name="$3"         # nome del .bin prodotto
    local elfname="$4"          # nome del binario ELF (dal Cargo.toml)
    local extra_flags="${5:-}"  # flag cargo extra opzionali (es. --no-default-features)

    local LD_ABS="$(pwd)/$ld"
    export CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS="\
-C relocation-model=pic \
-C link-arg=-T${LD_ABS} \
-C link-arg=--apply-dynamic-relocs"

    echo "[build] $elfname (freestanding, PIC @ USER_CODE)"
    # shellcheck disable=SC2086
    cargo build --release --manifest-path "$crate/Cargo.toml" $extra_flags

    local ELF="$TARGET_DIR/x86_64-unknown-none/release/$elfname"
    local OUT="$BUILD/$out_name"
    mkdir -p "$BUILD"
    llvm-objcopy -O binary --set-section-flags .bss=alloc,load,contents "$ELF" "$OUT"

    local SIZE=$(stat -c %s "$OUT")
    echo "[build] $OUT ($SIZE bytes)"
}
