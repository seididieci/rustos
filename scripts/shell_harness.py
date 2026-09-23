#!/usr/bin/env python3
"""Harness comune dei test interattivi di shell (estratto da test-shell.py).

Un boot QEMU per file di test: seriale su file + monitor su unix socket,
comandi via sendkey, assert sul log seriale (la shell specchia l'output).
Ogni file di fase (test-shell-*.py) e' autonomo: prepara le immagini, boota,
digita, verifica, pulisce. Il runner test-shell-all.sh li orchestra
(sequenziale o --jobs N con overlay qcow2 per istanza).

I nomi sendkey sono VERIFICATI su QEMU 10.2.2 (vedi KEYMAP sotto; dettagli e
sonde in smoke41.py).
"""
import socket, subprocess, sys, time, os, re

KERNEL_DFLT = "target/x86_64-unknown-none/release/velordor-kernel"
FAT_DFLT = "userland/fs/fat.img"
# Secondo disco (stessa fixture di run.sh: UUID C0FFEE01, label SECOND):
# senza, t36 fallisce per ambiente (mount UUID impossibile).
FAT2_DFLT = "userland/fs/fat2.img"

# Nomi sendkey VERIFICATI su QEMU 10.2.2 ('period' NON esiste: il punto e'
# 'dot'; MAIUSCOLE come combo 'shift-x'; '>' e '<' = shift-dot/shift-comma;
# nomi parser Fase 41 sondati live in smoke41.py; '\\' e '|' richiedono il
# fix Us104Fix in usertty).
KEYMAP = {" ": "spc", ".": "dot", "-": "minus", "/": "slash", "&": "shift-7",
          ">": "shift-dot", "<": "shift-comma", "!": "shift-1",
          "'": "apostrophe", '"': "shift-apostrophe",
          "\\": "backslash", "|": "shift-backslash",
          ";": "semicolon", "$": "shift-4", "*": "shift-8",
          "?": "shift-slash", "#": "shift-3", "~": "shift-grave_accent",
          "=": "equal", "_": "shift-minus", ":": "shift-semicolon",
          "{": "shift-bracket_left", "}": "shift-bracket_right",
          "`": "grave_accent"}
KEYMAP.update({chr(c): "shift-%s" % chr(c).lower() for c in range(ord("A"), ord("Z") + 1)})


def parse_shell_args(mon_dflt, serial_dflt):
    """CLI minima dei file di fase: --mon/--serial/--fat/--fat2/--kernel/
    --fat-format/--no-prep (il runner parallelo li passa per isolare le
    istanze; in locale valgono i default)."""
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--mon", default=mon_dflt)
    ap.add_argument("--serial", default=serial_dflt)
    ap.add_argument("--fat", default=FAT_DFLT)
    ap.add_argument("--fat2", default=FAT2_DFLT)
    ap.add_argument("--kernel", default=KERNEL_DFLT)
    ap.add_argument("--fat-format", default="raw",
                    help="raw (default) o qcow2 per gli overlay paralleli")
    ap.add_argument("--no-prep", action="store_true",
                    help="salta mkfat+inject (il runner li fa una volta sola)")
    return ap.parse_args()


def prep_images(fat=FAT_DFLT, fat2=FAT2_DFLT):
    """Rigenera le immagini FAT + inietta /bin e /test (come run.sh)."""
    subprocess.run(["python3", "scripts/mkfat.py", fat], check=True)
    subprocess.run(["python3", "scripts/mkfat.py", fat2,
                    "--serial", "C0FFEE01", "--label", "SECOND",
                    "--marker", "second disk marker"], check=True)
    subprocess.run(["bash", "scripts/inject-bins.sh"], check=True)


class Checker:
    """Raccoglie PASS/FAIL con stampa immediata (come il vecchio main)."""
    def __init__(self):
        self.ok = True

    def check(self, name, cond):
        print(("PASS " if cond else "FAIL ") + name, flush=True)
        self.ok = self.ok and bool(cond)
        return bool(cond)


class Shell:
    """Una istanza QEMU + shell Velordor. I path distinguono le istanze
    parallele (il runner li assegna per fase)."""

    def __init__(self, mon, serial, fat=FAT_DFLT, fat2=FAT2_DFLT,
                 kernel=KERNEL_DFLT, fat_format="raw"):
        self.mon = mon
        self.serial = serial
        self.fat = fat
        self.fat2 = fat2
        self.kernel = kernel
        self.fat_format = fat_format
        self.q = None
        # wait_prompt event-driven sul conteggio prompt CONSUMATO.
        self._prompt_seen = 0
        self._need_sync = False

    # ── Monitor / typing ──────────────────────────────────────────

    def send_mon(self, cmd: str, sleep=0.12):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(self.mon)
        s.sendall(cmd.encode() + b"\n")
        time.sleep(0.03)
        try:
            s.recv(4096)
        except Exception:
            pass
        s.close()
        time.sleep(sleep)

    def mon_query(self, cmd: str) -> str:
        """Interroga il monitor HMP e ritorna la risposta (diagnostica)."""
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(self.mon)
            s.sendall(cmd.encode() + b"\n")
            time.sleep(0.3)
            try:
                data = s.recv(65536)
            except Exception:
                data = b""
            s.close()
            return data.decode(errors="replace")
        except Exception as e:
            return "<mon query failed: %s>" % e

    def type_text(self, text: str, sleep=0.18):
        # Sleep generoso: a 8 tasti/s il guest perde scancode sotto carico
        # (buffer PS/2 a 1 byte, drain IRQ1+schedule ~40-60ms — overrun).
        # Pausa di drain ogni 12 tasti (i comandi lunghi troncavano la coda).
        for i, ch in enumerate(text):
            self.send_mon("sendkey %s" % KEYMAP.get(ch, ch), sleep=sleep)
            if (i + 1) % 12 == 0:
                time.sleep(1.0)

    def read_log(self):
        try:
            with open(self.serial, "rb") as f:
                return f.read()
        except FileNotFoundError:
            return b""

    # ── Prompt sync ───────────────────────────────────────────────

    def wait_prompt(self, timeout=8):
        t0 = time.time()
        if self._need_sync:
            deadline = time.time() + timeout
            while (self.read_log().count(b"$ ") <= self._prompt_seen
                    and time.time() < deadline):
                time.sleep(0.2)
            self._need_sync = False
        time.sleep(0.3)
        self._prompt_seen = self.read_log().count(b"$ ")
        dt = time.time() - t0
        if dt > 3:
            print("info slow wait_prompt %.1fs" % dt, flush=True)

    def run(self, cmd: str, sleep=1.0):
        t0 = time.time()
        self.wait_prompt()
        self.type_text(cmd)
        self.send_mon("sendkey ret")
        time.sleep(sleep)
        self._need_sync = True  # comando in volo: il prossimo wait sincronizza
        dt = time.time() - t0
        if dt > 10:
            print("info slow run %.1fs: %s" % (dt, cmd), flush=True)

    def run_out(self, cmd: str, sleep=1.0):
        """Esegue e ritorna SOLO l'output nuovo (coda del log): serve per
        gli assert di assenza (il log cumulativo contiene gia' tutto)."""
        t0 = time.time()
        self.wait_prompt()
        mark = len(self.read_log())
        self.type_text(cmd)
        self.send_mon("sendkey ret")
        time.sleep(sleep)
        self._need_sync = True  # come run()
        dt = time.time() - t0
        if dt > 10:
            print("info slow run_out %.1fs: %s" % (dt, cmd), flush=True)
        return self.read_log()[mark:]

    def run_heredoc(self, first: str, lines, delim: str, sleep=1.5):
        """Heredoc Fase 42: prima riga, corpo riga per riga (prompt secondario
        `> `, non contato dai prompt), delimitatore."""
        self.wait_prompt()
        mark = len(self.read_log())
        self.type_text(first)
        self.send_mon("sendkey ret")
        time.sleep(0.8)
        for ln in lines:
            self.type_text(ln)
            self.send_mon("sendkey ret")
            time.sleep(0.5)
        self.type_text(delim)
        self.send_mon("sendkey ret")
        time.sleep(sleep)
        self._need_sync = True
        return self.read_log()[mark:]

    # ── Boot / teardown ───────────────────────────────────────────

    def boot(self, timeout=60):
        """Avvia QEMU e attende la shell pronta (produzione: niente test,
        shell subito). Ritorna il Popen (anche in self.q)."""
        for p in (self.serial, self.mon):
            try:
                os.unlink(p)
            except FileNotFoundError:
                pass
        args = [
            "qemu-system-x86_64", "-m", "256M", "-display", "none",
            "-serial", "file:" + self.serial,
            "-monitor", "unix:%s,server=on,wait=off" % self.mon,
            "-no-reboot", "-kernel", self.kernel,
            "-drive", "file=%s,format=%s,if=ide" % (self.fat, self.fat_format),
            "-drive", "file=%s,format=%s,if=ide" % (self.fat2, self.fat_format),
        ]
        # KVM se disponibile: abbatte la tassa di emulazione sui round-trip
        # tastiera (IRQ1→kbd→tty→shell→prompt).
        if os.path.exists("/dev/kvm"):
            args[1:1] = ["-accel", "kvm"]
        self.q = subprocess.Popen(args)
        time.sleep(1.0)
        kvm_info = self.mon_query("info kvm").replace("\r", "").strip().split("\n")
        kvm_tail = [l for l in kvm_info if "kvm" in l.lower()][-1:]
        print("info accel: %s" % (" | ".join(kvm_tail) if kvm_tail else kvm_info[-1:]),
              flush=True)
        deadline = time.time() + timeout
        while time.time() < deadline:
            data = self.read_log()
            if (b"[shell] starting" in data
                    and b"registered mount '/dev/input'" in data
                    and b"$ " in data):
                self.wait_prompt()
                return self.q
            time.sleep(0.4)
        raise RuntimeError("shell non pronta")

    def terminate(self):
        if self.q is not None:
            self.q.terminate()
            try:
                self.q.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.q.kill()
            self.q = None

    # ── VGA screendump (test backspace/clear) ─────────────────────

    def screendump(self, path: str):
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
        self.send_mon("screendump %s" % path, sleep=0.6)
        deadline = time.time() + 10
        while time.time() < deadline:
            if os.path.exists(path) and os.path.getsize(path) > 1000:
                time.sleep(0.3)
                return True
            time.sleep(0.2)
        return False


def load_ppm(path: str):
    with open(path, "rb") as f:
        data = f.read()
    assert data[:2] == b"P6", "screendump non PPM"
    # Header: P6 + commenti + "W H" + maxval, poi byte raw RGB.
    parts = []
    i = 2
    while len(parts) < 3:
        while data[i:i+1].isspace():
            i += 1
        if data[i:i+1] == b"#":
            while data[i:i+1] != b"\n":
                i += 1
            continue
        j = i
        while not data[j:j+1].isspace():
            j += 1
        parts.append(data[i:j])
        i = j
    while data[i:i+1].isspace():
        i += 1
    w, h = int(parts[0]), int(parts[1])
    return w, h, data[i:i + w * h * 3]


def lit_pixels(path: str):
    """Byte accesi nello screendump (testo bianco su nero), escluse le
    ultime 3 scanline (cursore lampeggiante)."""
    w, h, p = load_ppm(path)
    row = w * 3
    lit = 0
    for y in range(h - 3):
        off = y * row
        for k in range(row):
            if p[off + k] != 0:
                lit += 1
    return lit


def diff_masked(a: str, b: str):
    """Byte diversi tra due screendump, mascherando le ultime 3 scanline
    (cursore hardware lampeggiante: non e' contenuto)."""
    wa, ha, pa = load_ppm(a)
    wb, hb, pb = load_ppm(b)
    if (wa, ha, len(pa)) != (wb, hb, len(pb)):
        return -1
    row = wa * 3
    diff = 0
    for y in range(ha - 3):
        off = y * row
        for k in range(row):
            if pa[off + k] != pb[off + k]:
                diff += 1
    return diff
