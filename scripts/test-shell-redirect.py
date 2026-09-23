#!/usr/bin/env python3
"""Test shell REDIRECT (Fase 40.4): >, >>, <, 2>, 2>>, 2>&1 ordinati + run redirectato."
Avvia il proprio QEMU (seriale + monitor dedicati), digita via sendkey,
verifica sul log seriale. Autonomo: prepara le immagini (salvo --no-prep),
boota, testa, pulisce le sue fixture. Vedi scripts/shell_harness.py.
"""
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
from shell_harness import Shell, Checker, prep_images, parse_shell_args
def main():
    args = parse_shell_args("/tmp/velordor-redir-mon.sock", "/tmp/velordor-redir-serial.log")
    if not args.no_prep:
        prep_images()
    sh = Shell(mon=args.mon, serial=args.serial, fat=args.fat, fat2=args.fat2,
               kernel=args.kernel, fat_format=args.fat_format)
    c = Checker()
    try:
        sh.boot()
        # Fase 40.4: redirect shell (bash-like: ultimo vince per slot).
        # Builtin stdout: > crea/tronca, >> appende.
        sh.run("echo hello redir > redir404.txt")
        out = sh.run_out("cat redir404.txt")
        found = b"hello redir" in out
        c.check("echo > file + cat", found)
        sh.run("echo second >> redir404.txt")
        out = sh.run_out("cat redir404.txt")
        found = b"hello redir" in out and b"second" in out
        c.check(">> appende", found)
        sh.run("echo solo > redir404.txt")
        out = sh.run_out("cat redir404.txt")
        found = b"solo" in out and b"second" not in out
        c.check("> tronca", found)

        # Stdin builtin: cat/wc/hexdump senza file leggono <.
        out = sh.run_out("cat < redir404.txt")
        found = b"solo" in out
        c.check("cat < file", found)
        out = sh.run_out("wc < redir404.txt")
        found = b"1 1 5 -" in out
        c.check("wc < file (=1 1 5 -)", found)
        out = sh.run_out("hexdump < redir404.txt")
        found = b"73 6f 6c 6f" in out
        c.check("hexdump < file", found)
        out = sh.run_out("cat < /no404dir")
        found = b"no such file or directory" in out
        c.check("< missing (ENOENT distinto)", found)

        # Separazione stdout/stderr: l'errore non inquina >.
        sh.run("cat missing404 > /o404.txt")
        out = sh.run_out("ls -l")
        found = b"- 0 o404.txt" in out
        c.check("errore non inquina > (file vuoto)", found)
        out = sh.run_out("cat missing404 2> /e404.txt")
        out = sh.run_out("cat /e404.txt")
        found = b"cannot open missing404" in out
        c.check("2> cattura errore builtin", found)
        out = sh.run_out("cat missing404 2>> /e404b.txt")
        out = sh.run_out("cat /e404b.txt")
        found = b"cannot open missing404" in out
        c.check("2>> appende errore builtin", found)
        sh.run("cat missing404 > /o404b.txt 2>&1")
        out = sh.run_out("cat /o404b.txt")
        found = b"cannot open missing404" in out
        c.check("2>&1 dopo >: errore nel file", found)
        out = sh.run_out("cat missing404 2>&1 > /o404c.txt")
        found = b"cannot open missing404" in out
        c.check("2>&1 prima di >: errore su terminale", found)
        out = sh.run_out("ls -l")
        found = b"- 0 o404c.txt" in out
        c.check("2>&1 prima di >: file vuoto", found)
        out = sh.run_out("echo hi >")
        found = b"missing target" in out
        c.check("redirect senza target", found)

        # run con redirect (handoff grant via argv-magic, claim nello startup).
        sh.run("run /fat/bin/runhello.bin hello > /ro404.txt")
        out = sh.run_out("cat /ro404.txt")
        found = b"runhello: hello" in out and b"non utf8" not in out
        c.check("run > file (magic nascosto)", found)
        out = sh.run_out("run /fat/bin/runhello.bin fail > /ro404b.txt")
        found = b"[exit 3]" in out
        c.check("run > file + exit code", found)
        sh.run("run /fat/bin/runhello.bin < redir404.txt > /ro404c.txt")
        out = sh.run_out("cat /ro404c.txt")
        found = b"runhello: stdin:solo" in out
        c.check("run < > : stdin nel file", found)
        out = sh.run_out("run /fat/bin/runhello.bin bgx > /rb404.txt &")
        found = b"[bg pid" in out
        c.check("run bg + redirect", found)
        out = sh.run_out("wait", sleep=2.0)
        found = b"exit" in out
        c.check("wait chiude bg redirectato", found)
        out = sh.run_out("cat /rb404.txt")
        found = b"runhello: bgx" in out
        c.check("output bg nel file", found)
        for _cmd in ["rm redir404.txt", "rm /o404.txt", "rm /o404b.txt",
                       "rm /o404c.txt", "rm /e404.txt", "rm /e404b.txt",
                       "rm /ro404.txt", "rm /ro404b.txt", "rm /ro404c.txt",
                       "rm /rb404.txt"]:
            sh.run(_cmd)
        return 0 if c.ok else 1
    except RuntimeError as e:
        print("FAIL: %s" % e)
        return 1
    finally:
        sh.terminate()


if __name__ == "__main__":
    sys.exit(main())
