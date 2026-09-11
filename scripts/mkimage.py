#!/usr/bin/env python3
"""Compila l'immagine disco avviabile di Velordor.

Pipeline:
  1. kernel ELF64 (cargo build) --objcopy--> payload.bin (flat, base 1 MiB)
  2. scripts/boot.asm           --nasm-----> settore di boot (512 B)
  3. immagine disco = settore di boot + payload (padded ai settori)

Il contatore dei settori payload viene patchato nel boot sector tramite il
magic "PSCNT". QEMU avvia l'immagine come disco rigido standard (SeaBIOS).
"""

import subprocess
import sys
from pathlib import Path

SCRIPTS = Path(__file__).parent


def run(cmd: list[str]) -> None:
    print("+", " ".join(cmd))
    subprocess.run(cmd, check=True)


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <kernel.elf> <output.img>")

    elf_path, img_path = Path(sys.argv[1]), Path(sys.argv[2])
    out_path = img_path.parent
    out_path.mkdir(parents=True, exist_ok=True)

    # 1) ELF64 -> binario piatto (i byte finiscono alle VMA, base = 1 MiB)
    payload_path = out_path / "payload.bin"
    run(["objcopy", "-O", "binary", str(elf_path), str(payload_path)])
    payload = payload_path.read_bytes()

    # padding all'ultimo settore
    pad = (-len(payload)) % 512
    payload += b"\x00" * pad
    sectors = len(payload) // 512
    assert sectors < 0xFFFF, "payload troppo grande"

    # 2) boot sector
    boot_bin = out_path / "boot_sector.bin"
    run([
        "nasm", "-f", "bin",
        "-I", f"{SCRIPTS}/",
        str(SCRIPTS / "boot.asm"),
        "-o", str(boot_bin),
    ])
    boot = bytearray(boot_bin.read_bytes())
    assert len(boot) == 512, f"boot sector {len(boot)} != 512"

    # 3) patch numero settori payload
    magic = boot.find(b"PSCNT")
    assert magic != -1, "magic PSCNT non trovato nel boot sector"
    boot[magic + 5:magic + 7] = sectors.to_bytes(2, "little")

    img_path.write_bytes(bytes(boot) + payload)
    print(f"mkimage: {img_path} ({512 + len(payload)} bytes, "
          f"payload {sectors} settori)")


if __name__ == "__main__":
    main()
