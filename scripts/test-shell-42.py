#!/usr/bin/env python3
"""Test shell PIPE (Fase 42): a | b, N stadi, redirect+pipe, waitpid status, stadi run, bg rifiutata, heredoc, EOF, streaming oltre capacita'."
Avvia il proprio QEMU (seriale + monitor dedicati), digita via sendkey,
verifica sul log seriale. Autonomo: prepara le immagini (salvo --no-prep),
boota, testa, pulisce le sue fixture. Vedi scripts/shell_harness.py.
"""
import sys, os, re
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
from shell_harness import Shell, Checker, prep_images, parse_shell_args
def main():
    args = parse_shell_args("/tmp/velordor-42-mon.sock", "/tmp/velordor-42-serial.log")
    if not args.no_prep:
        prep_images()
    sh = Shell(mon=args.mon, serial=args.serial, fat=args.fat, fat2=args.fat2,
               kernel=args.kernel, fat_format=args.fat_format)
    c = Checker()
    try:
        sh.boot()
        # Pipe base builtin|builtin + 3 stadi.
        out = sh.run_out("echo hello42 | cat")
        found = b"hello42" in out
        c.check("42 pipe base (echo | cat)", found)
        # cat aggiunge sempre un \n finale: il file da 25B/1 riga diventa
        # 26B/2 righe nella pipe (coerente col redirect su file).
        out = sh.run_out("cat hello.txt | wc")
        found = b"2 4 26 -" in out
        c.check("42 cat | wc (2 4 26)", found)
        out = sh.run_out("echo a | echo b | cat")
        found = b"b" in out and b"[exit" not in out
        c.check("42 pipe 3 stadi", found)

        # Pipe + redirect combinati (esplicito vince sul link).
        sh.run("echo comb42 | cat > /po42.txt")
        out = sh.run_out("cat /po42.txt")
        found = b"comb42" in out
        c.check("42 pipe + > file", found)
        sh.run("rm /po42.txt")

        # Errore nello stadio sinistro: messaggio sul terminale, destro a EOF.
        out = sh.run_out("cat missing404 | cat")
        found = b"cannot open missing404" in out
        c.check("42 errore stadio sx", found)

        # waitpid con status: status del gruppo = ultimo stadio (bash).
        sh.run("cat missing404 | echo hi")
        out = sh.run_out("echo $?")
        found = out.split(b"\n")[0].strip() == b"0"
        c.check("42 status = ultimo stadio (0)", found)
        out = sh.run_out("echo hi | cat missing404")
        found = b"[exit 1]" in out
        c.check("42 status ultimo fallito ([exit 1])", found)
        out = sh.run_out("echo $?")
        found = out.split(b"\n")[0].strip() == b"1"
        c.check("42 $? dopo pipe fallita (=1)", found)

        # Stadio run (fork+exec con grant, non builtin).
        out = sh.run_out("run /fat/bin/runhello.bin hello | cat")
        found = b"runhello: hello" in out
        c.check("42 stadio run | cat", found)
        out = sh.run_out("run /fat/bin/runhello.bin fail | cat missing404")
        found = b"[exit 1]" in out
        c.check("42 run fallito in pipe ([exit 1])", found)

        # & su pipeline multi-stadio: Fase 44 (rifiuto chiaro, mai hang).
        out = sh.run_out("echo a | cat &")
        found = b"Fase 44" in out
        c.check("42 bg pipeline rifiutata", found)
        # Pipe trailing ignorata come gli altri connettori (mai errore).
        out = sh.run_out("echo tp42 |")
        found = b"tp42" in out
        c.check("42 pipe trailing ignorata", found)

        # Heredoc base: corpo letterale nello stdin di cat.
        out = sh.run_heredoc("cat <<EOF42", ["riga uno", "riga due"], "EOF42")
        found = b"riga uno" in out and b"riga due" in out
        c.check("42 heredoc base", found)
        # Heredoc + pipe: corpo + \n finale di cat ("aa bb\ncc\n" -> 3 3 10,
        # come `cat hello.txt | wc`: cat aggiunge sempre un \n in coda).
        out = sh.run_heredoc("cat <<EOF43 | wc", ["aa bb", "cc"], "EOF43")
        found = b"3 3 10 -" in out
        c.check("42 heredoc | wc (3 3 10)", found)
        # Heredoc + redirect file.
        sh.run_heredoc("cat <<EOF44 > /hd42.txt", ["salvata"], "EOF44")
        out = sh.run_out("cat /hd42.txt")
        found = b"salvata" in out
        c.check("42 heredoc > file", found)
        sh.run("rm /hd42.txt")

        # EOF vero: file vuoto in pipe (mai hang su Empty). cat stampa comunque
        # il suo \n finale: la pipe vede "\n" = 1 0 1 (l'EOF e' propagato).
        sh.run("touch /empty42")
        out = sh.run_out("cat /empty42 | wc")
        found = b"1 0 1 -" in out
        c.check("42 pipe EOF (1 0 1)", found)
        sh.run("rm /empty42")

        # Streaming oltre la capacita' pipe (8192): raddoppi via >> poi pipe.
        sh.run("echo x0123456789abcdef | cat > /big42a.txt")
        sh.run("cp /big42a.txt /big42.txt")
        for _ in range(9):
            sh.run("cat /big42.txt >> /big42a.txt")
            sh.run("cp /big42a.txt /big42.txt")
        out = sh.run_out("cat /big42.txt | wc")
        m = re.search(rb"(\d+) (\d+) (\d+) -", out)
        # Atteso >> cap (seed 19B con \n di echo+cat, 9 raddoppi + \n di cat
        # a ogni giro: 10239B): conta linee/parole/byte.
        found = m is not None and int(m.group(3)) > 8192
        c.check("42 pipe oltre capacita' (>8192B)", found)
        out = sh.run_out("wc /big42.txt")
        m2 = re.search(rb"(\d+) (\d+) (\d+) /big42", out)
        # La pipe vede file + \n finale di cat: linee+1, parole uguali, byte+1.
        found = (m is not None and m2 is not None
                 and int(m.group(1)) == int(m2.group(1)) + 1
                 and m.group(2) == m2.group(2)
                 and int(m.group(3)) == int(m2.group(3)) + 1)
        c.check("42 pipe == file + newline cat", found)
        sh.run("rm /big42.txt")
        sh.run("rm /big42a.txt")
        return 0 if c.ok else 1
    except RuntimeError as e:
        print("FAIL: %s" % e)
        return 1
    finally:
        sh.terminate()


if __name__ == "__main__":
    sys.exit(main())
