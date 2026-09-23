#!/usr/bin/env python3
"""Test shell PARSER (Fase 41): quote/escape/commenti, ; && ||, $VAR/$?/~/$$, export, field-split, glob, errori."
Avvia il proprio QEMU (seriale + monitor dedicati), digita via sendkey,
verifica sul log seriale. Autonomo: prepara le immagini (salvo --no-prep),
boota, testa, pulisce le sue fixture. Vedi scripts/shell_harness.py.
"""
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
from shell_harness import Shell, Checker, prep_images, parse_shell_args
def main():
    args = parse_shell_args("/tmp/velordor-41-mon.sock", "/tmp/velordor-41-serial.log")
    if not args.no_prep:
        prep_images()
    sh = Shell(mon=args.mon, serial=args.serial, fat=args.fat, fat2=args.fat2,
               kernel=args.kernel, fat_format=args.fat_format)
    c = Checker()
    try:
        sh.boot()
        # Fase 41: parser quote-aware (quote/escape/commenti, ; && || &,
        # $VAR ${VAR} $? $$ ~, glob * ?; pipe rifiutata verso Fase 42).
        # Quote: singolo raggruppa e inibisce tutto, doppio solo $.
        out = sh.run_out("echo 'a   b'")
        found = b"a   b" in out
        c.check("41 single-quote raggruppa", found)
        out = sh.run_out("echo '>'")
        found = b">" in out
        c.check("41 redirect in quote letterale", found)
        out = sh.run_out("echo 'a#b'")
        found = b"a#b" in out
        c.check("41 # in quote non e' commento", found)
        sh.run("export Q41=vv")
        out = sh.run_out("echo '$Q41'")
        found = b"$Q41" in out and b"vv" not in out
        c.check("41 $ in single-quote letterale", found)
        out = sh.run_out("echo '*'")
        found = b"*" in out
        c.check("41 glob in quote inibito", found)
        out = sh.run_out('echo "a   b"')
        found = b"a   b" in out
        c.check("41 double-quote raggruppa", found)
        out = sh.run_out('echo "v=$Q41"')
        found = b"v=vv" in out
        c.check("41 $ in double-quote espande", found)
        out = sh.run_out('echo "a\\$B"')
        found = b"a$B" in out
        c.check("41 escape in double-quote", found)

        # Escape fuori quote + commenti.
        out = sh.run_out("echo a\\ b")
        found = b"a b" in out
        c.check("41 escape spazio", found)
        out = sh.run_out("echo a\\;b")
        found = b"a;b" in out
        c.check("41 escape punto-e-virgola", found)
        out = sh.run_out("echo hi # trailing")
        found = b"hi" in out
        c.check("41 commento trailing", found)

        # Variabili: bare-assign, ${}, unset, $$, ~, export lista/errori.
        sh.run("BARE41=zzz")
        out = sh.run_out("echo $BARE41")
        found = b"zzz" in out
        c.check("41 bare NAME=valore", found)
        out = sh.run_out("echo ${BARE41}!")
        found = b"zzz!" in out
        c.check("41 ${VAR}", found)
        out = sh.run_out("echo pre$UNSET41Xpost")
        found = b"UNSET41X" not in out and out.split(b"\n")[0].strip() == b"pre"
        c.check("41 $UNSET sparisce", found)
        out = sh.run_out("echo $$")
        found = re.search(rb"\d+", out) is not None and b"$$" not in out
        c.check("41 $$ numerico", found)
        out = sh.run_out("echo ~")
        found = out.split(b"\n")[0].strip() == b"/"
        c.check("41 tilde -> /", found)
        out = sh.run_out("export")
        found = b"BARE41=zzz" in out
        c.check("41 export lista", found)
        out = sh.run_out("export 1BAD41=x")
        found = b"bad name" in out
        c.check("41 export nome invalido", found)
        out = sh.run_out("F41X=1 echo hi")
        found = b"non supportato" in out
        c.check("41 VAR=v cmd rifiutato (Fase 43)", found)
        # Field-split: una variabile con spazio diventa DUE argv (osservabile
        # via `cp src dst`: senza split sarebbe un'unica sorgente inesistente).
        sh.run("echo spcontent > /sp41.txt")
        sh.run('export F41="/sp41.txt /sp41c.txt"')
        sh.run("cp $F41")
        out = sh.run_out("cat /sp41c.txt")
        found = b"spcontent" in out
        c.check("41 field-split non quotato", found)
        sh.run("rm /sp41.txt")
        sh.run("rm /sp41c.txt")

        # Connettori: ; && ||, short-circuit, catene, $?, ignoto=127.
        out = sh.run_out("echo c41a; echo c41b")
        found = b"c41a" in out and b"c41b" in out
        c.check("41 ; sequenza", found)
        out = sh.run_out("cat missing41; echo after41")
        found = b"after41" in out
        c.check("41 ; ignora lo status", found)
        out = sh.run_out("echo ok41 && echo yes41")
        found = b"ok41" in out and b"yes41" in out
        c.check("41 && catena", found)
        out = sh.run_out("cat missing41 && echo no41")
        found = b"no41" not in out
        c.check("41 && short-circuit", found)
        out = sh.run_out("cat missing41 || echo or41")
        found = b"or41" in out
        c.check("41 || scatta", found)
        out = sh.run_out("echo ok41b || echo no41b")
        found = b"ok41b" in out and b"no41b" not in out
        c.check("41 || salta a successo", found)
        out = sh.run_out("cat missing41 || cat missing41b || echo deep41")
        found = b"deep41" in out
        c.check("41 catena || profonda", found)
        sh.run("cat missing41")
        out = sh.run_out("echo $?")
        found = b"1" in out
        c.check("41 $? dopo errore", found)
        out = sh.run_out("nosuchcmd41")
        found = b"unknown command" in out
        c.check("41 comando ignoto", found)
        out = sh.run_out("echo $?")
        found = b"127" in out
        c.check("41 $? dopo ignoto (=127)", found)
        out = sh.run_out("echo comb41 > /comb41.txt && cat /comb41.txt")
        found = b"comb41" in out
        c.check("41 redirect + &&", found)
        sh.run("rm /comb41.txt")

        # Glob via readdir: *, ?, no-match letterale, dotfile esclusi.
        sh.run("touch g41a1")
        sh.run("touch g41a2")
        sh.run("touch g41b1")
        out = sh.run_out("echo g41*")
        found = b"g41a1" in out and b"g41a2" in out and b"g41b1" in out
        c.check("41 glob *", found)
        out = sh.run_out("echo g41a?")
        found = b"g41a1" in out and b"g41a2" in out and b"g41b1" not in out
        c.check("41 glob ?", found)
        out = sh.run_out("echo g41nomatch*.zzz")
        found = b"g41nomatch*.zzz" in out
        c.check("41 glob no-match letterale", found)
        sh.run("touch .h41")
        out = sh.run_out("echo *")
        found = b"h41" not in out
        c.check("41 glob esclude dotfile", found)
        out = sh.run_out("echo .h*")
        found = b".h41" in out
        c.check("41 glob dotfile con punto", found)
        sh.run("rm g41a1")
        sh.run("rm g41a2")
        sh.run("rm g41b1")
        sh.run("rm .h41")
        out = sh.run_out('echo "abc')
        found = b"abc" in out
        c.check("41 quote non chiusa letterale", found)
        return 0 if c.ok else 1
    except RuntimeError as e:
        print("FAIL: %s" % e)
        return 1
    finally:
        sh.terminate()


if __name__ == "__main__":
    sys.exit(main())
