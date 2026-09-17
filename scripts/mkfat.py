#!/usr/bin/env python3
"""Genera un'immagine disco FAT32 minimale per Velordor.

Produce `fat.img` da montare come `/fat` dal fs server (Fase 9.2, scrivibile
da Fase 20): BPB corretto, 2 FAT identiche, root directory nel cluster 2,
file 8.3 con catena di cluster e una sotto-directory.

Parametri (coerenti con userland/fs/src/fat32.rs):
  settore 512 B, cluster 4 KiB (spc=8), rsvd=32, num_fats=2, fat_size=516 settori
  => data_start = 32 + 2*516 = 1064.

Nessuna dipendenza da tool host: scrive i byte direttamente.
"""

import struct
import sys
from pathlib import Path

BYTES_PER_SEC = 512
SPC = 8                 # settori per cluster (cluster = 4 KiB)
RSVD_SEC = 32
NUM_FATS = 2
TOTAL_SEC = 527000      # ~257 MiB -> >= 65525 cluster (minimo FAT32)
# FAT abbastanza grande per coprire i cluster: 66048 entry (516 settori).
FAT_SIZE_SEC = 516
ROOT_CLUSTER = 2
VOL_LABEL = b"VELORDOR   "
VOL_SERIAL = 0x4F4C4556  # "VELO" LE: UUID stabile del disco di boot (16d)
SPT = 63                # settori per traccia (CHS, solo per compatibilita' tool)
HEADS = 255

DATA_START = RSVD_SEC + NUM_FATS * FAT_SIZE_SEC      # 32 + 2*516 = 1064
DATA_SECS = TOTAL_SEC - DATA_START
TOTAL_CLUSTERS = DATA_SECS // SPC
EOC = 0x0FFFFFFF


class Builder:
    def __init__(self, vol_serial=VOL_SERIAL, vol_label=VOL_LABEL):
        self.img = bytearray(BYTES_PER_SEC * TOTAL_SEC)
        self.next_cluster = ROOT_CLUSTER + 1
        self.fat = [0] * (ROOT_CLUSTER + TOTAL_CLUSTERS)
        self.fat[ROOT_CLUSTER] = EOC                 # catena della root dir
        self.vol_serial = vol_serial
        self.vol_label = vol_label

    def alloc_chain(self, length_bytes: int) -> list:
        clusters = []
        needed = (length_bytes + BYTES_PER_SEC * SPC - 1) // (BYTES_PER_SEC * SPC)
        needed = max(needed, 1)
        for _ in range(needed):
            clusters.append(self.next_cluster)
            self.next_cluster += 1
        for i, c in enumerate(clusters):
            self.fat[c] = EOC if i == len(clusters) - 1 else clusters[i + 1]
        return clusters

    def cluster_off(self, cluster: int) -> int:
        return DATA_START * BYTES_PER_SEC + (cluster - ROOT_CLUSTER) * BYTES_PER_SEC * SPC

    def write_clusters(self, clusters: list, data: bytes):
        off = self.cluster_off(clusters[0])
        self.img[off:off + len(data)] = data

    def dir_entry_bytes(self, path: str, first_cluster: int, size: int, attr: int = 0x20) -> bytes:
        if path == ".":
            name8, ext3 = b".       ", b"   "
        elif path == "..":
            name8, ext3 = b"..      ", b"   "
        else:
            name, _, ext = path.upper().partition(".")
            name8 = (name + "        ")[:8].encode()
            ext3 = (ext + "   ")[:3].encode()
        e = name8 + ext3
        e += bytes([attr])
        e += bytes(8)                                 # NT/crt/atime riservati
        e += struct.pack("<H", (first_cluster >> 16) & 0xFFFF)   # clust hi
        e += struct.pack("<H", 0)                     # wr time
        e += struct.pack("<H", 0)                     # wr date
        e += struct.pack("<H", first_cluster & 0xFFFF)            # clust lo
        e += struct.pack("<I", size)
        assert len(e) == 32
        return e

    def add_dir_entry(self, dir_cluster: int, path: str, first_cluster: int,
                      size: int, attr: int = 0x20):
        dirstart = self.cluster_off(dir_cluster)
        entry = self.dir_entry_bytes(path, first_cluster, size, attr)
        for i in range(0, BYTES_PER_SEC * SPC, 32):
            pos = dirstart + i
            if self.img[pos] in (0x00, 0xE5):
                self.img[pos:pos + 32] = entry
                return
        raise SystemExit(f"directory piena: {path}")

    def add_file(self, path: str, content: bytes):
        clusters = self.alloc_chain(len(content))
        self.write_clusters(clusters, content)
        self.add_dir_entry(ROOT_CLUSTER, path, clusters[0], len(content))

    def add_subdir(self, name: str, files):
        """Sotto-directory: catena di cluster + entry nella root; le entry dei
        file vengono scritte nel primo cluster della subdir (con '.' e '..')."""
        sub = self.alloc_chain(BYTES_PER_SEC * SPC)
        self.add_dir_entry(ROOT_CLUSTER, name, sub[0], 0, attr=0x10)
        self.add_dir_entry(sub[0], ".", sub[0], 0, attr=0x10)
        self.add_dir_entry(sub[0], "..", ROOT_CLUSTER, 0, attr=0x10)
        for path, content in files:
            clusters = self.alloc_chain(len(content))
            self.write_clusters(clusters, content)
            self.add_dir_entry(sub[0], path, clusters[0], len(content))

    def _write_fat(self, fat_off: int):
        struct.pack_into("<I", self.img, fat_off + 0, 0x0FFFFFF8)
        struct.pack_into("<I", self.img, fat_off + 4, 0x0FFFFFFF)
        for c in range(ROOT_CLUSTER, self.next_cluster):
            if self.fat[c]:
                struct.pack_into("<I", self.img, fat_off + c * 4, self.fat[c])

    def _boot_sector(self):
        boot = bytearray(512)
        boot[0:3] = b"\xEB\x3C\x90"
        boot[3:11] = b"VELORDOR"
        struct.pack_into("<H", boot, 11, BYTES_PER_SEC)
        boot[13] = SPC
        struct.pack_into("<H", boot, 14, RSVD_SEC)
        boot[16] = NUM_FATS
        struct.pack_into("<H", boot, 17, 0)
        struct.pack_into("<H", boot, 19, 0)
        boot[21] = 0xF8
        struct.pack_into("<H", boot, 22, 0)
        struct.pack_into("<H", boot, 24, SPT)
        struct.pack_into("<H", boot, 26, HEADS)
        struct.pack_into("<I", boot, 28, 0)
        struct.pack_into("<I", boot, 32, TOTAL_SEC)
        struct.pack_into("<I", boot, 36, FAT_SIZE_SEC)
        struct.pack_into("<H", boot, 40, 0)
        struct.pack_into("<H", boot, 42, 0)
        struct.pack_into("<I", boot, 44, ROOT_CLUSTER)
        struct.pack_into("<H", boot, 48, 1)
        struct.pack_into("<H", boot, 50, 6)
        boot[64] = 0x80
        boot[65] = 0
        # Layout STANDARD (firma a 66, volid 67-70, label 71-81): il vecchio
        # layout (firma a 67, volid 68-71) faceva sovrapporre label[0] al 4°
        # byte del seriale per seriali arbitrari (16d). Il parser accetta
        # entrambi, il generatore emette solo questo.
        boot[66] = 0x29
        struct.pack_into("<I", boot, 67, self.vol_serial)
        boot[71:82] = self.vol_label
        boot[82:90] = b"FAT32   "
        boot[510:512] = b"\x55\xAA"
        self.img[0:512] = boot

    def _fsinfo(self):
        fsi = bytearray(512)
        fsi[0:4] = b"\x52\x52\x61\x41"
        fsi[484:488] = b"\x72\x72\x41\x61"
        struct.pack_into("<I", fsi, 488, TOTAL_CLUSTERS - (self.next_cluster - ROOT_CLUSTER))
        struct.pack_into("<I", fsi, 492, self.next_cluster)
        fsi[510:512] = b"\x55\xAA"
        self.img[1 * BYTES_PER_SEC:2 * BYTES_PER_SEC] = fsi

    def build(self) -> bytes:
        self._boot_sector()
        self._fsinfo()
        self._write_fat(RSVD_SEC * BYTES_PER_SEC)
        self._write_fat((RSVD_SEC + FAT_SIZE_SEC) * BYTES_PER_SEC)
        self.img[6 * BYTES_PER_SEC:7 * BYTES_PER_SEC] = self.img[0:512]
        return bytes(self.img)


def main() -> None:
    import argparse
    ap = argparse.ArgumentParser(description="Immagine FAT32 per Velordor (16d: seriale/label/marker parametrici)")
    ap.add_argument("out", nargs="?", default="userland/fs/fat.img")
    ap.add_argument("--serial", default="4F4C4556",
                    help="seriale volume esadecimale (UUID stabile, default 4F4C4556)")
    ap.add_argument("--label", default="VELORDOR",
                    help="label volume (max 11 char, default VELORDOR)")
    ap.add_argument("--marker", default=None, metavar="TESTO",
                    help="se dato, aggiunge il file MARKER.TXT con TESTO (disco secondario di test)")
    args = ap.parse_args()
    out = Path(args.out)
    serial = int(args.serial, 16)
    label = (args.label.upper() + " " * 11)[:11].encode()
    b = Builder(vol_serial=serial, vol_label=label)
    b.add_file("HELLO.TXT", b"Hello from Velordor FAT32!\n")
    b.add_file("README.TXT", b"Velordor FAT32 demo (Fase 9.2, scrivibile da Fase 20)\n")
    if args.marker is not None:
        b.add_file("MARKER.TXT", args.marker.encode())
    b.add_subdir("SUB", [("NOTES.TXT", b"Subdirectory note.\n")])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(b.build())
    print(f"mkfat: {out} ({out.stat().st_size} bytes, "
          f"clusters fino a {b.next_cluster - 1})")


if __name__ == "__main__":
    main()
