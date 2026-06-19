#!/bin/sh
# Fake `soteria-rust` used by integration tests to exercise cargo-soteria's
# parallel runner deterministically — without the real (slow, non-deterministic)
# analyzer. It mimics the two subcommands the runner uses:
#
#   compile --list-tests .   -> JSON array of entry points on stdout
#   exec . --filter ^NAME$   -> behaves according to NAME, with a matching exit
#                               code (0 pass / 1 fail / 2 soteria-crash /
#                               3 charon-crash; "anomaly" exits 0 without
#                               running; "slow" sleeps so a test can interrupt).
#
# The test list can be overridden via $FAKE_TEST_LIST.
#
# It is also `--kani`-aware (mirroring the real tool's mode flags): when invoked
# with `--kani`, `compile` lists kani harnesses instead, and `exec` prepends a
# `[kani]` marker — so a test can assert a forwarded flag reached both phases.

sub="$1"
shift

# Recover the test name from the anchored `--filter ^name$` argument, and note
# whether a `--kani` mode flag was forwarded to us.
filter=""
prev=""
has_kani=0
for a in "$@"; do
    [ "$prev" = "--filter" ] && filter="$a"
    [ "$a" = "--kani" ] && has_kani=1
    prev="$a"
done
name=$(printf '%s' "$filter" | tr -d '^$\\')

case "$sub" in
    compile)
        # Simulate a crate that fails to compile: the real soteria-rust prints
        # the rustc diagnostic to *stdout* and only "Compiling… errored" to
        # stderr, then exits 3. A `soteria-compile-error.txt` in the crate dir
        # supplies the canned diagnostic (see tests/fixtures/type-error/).
        if [ -f soteria-compile-error.txt ]; then
            echo "Compiling... errored" >&2
            cat soteria-compile-error.txt
            exit 3
        fi
        echo "Compiling... done" >&2
        if [ -n "$FAKE_TEST_LIST" ]; then
            printf '%s\n' "$FAKE_TEST_LIST"
        elif [ "$has_kani" = 1 ]; then
            printf '%s\n' '["kani::harness_a","kani::harness_b"]'
        else
            printf '%s\n' '["m::pass_one","m::pass_two","m::fail_one","m::crash_one","m::charon_one","m::anomaly_one"]'
        fi
        ;;
    exec)
        [ "$has_kani" = 1 ] && echo "[kani] $name"
        case "$name" in
            *anomaly*) exit 0 ;;                                   # exit 0 but nothing ran
            *pass*)   echo "=> Running $name..."; echo "note: ok"; exit 0 ;;
            *fail*)   echo "=> Running $name..."; echo "error: issues found"; exit 1 ;;
            *crash*)  echo "=> Running $name..."; echo "fatal: boom" >&2; exit 2 ;;
            *charon*) echo "charon exploded" >&2; exit 3 ;;
            *slow*)   echo "=> Running $name..."; sleep 30; exit 0 ;;
            *)        echo "=> Running $name..."; exit 0 ;;
        esac
        ;;
    *)
        echo "fake soteria-rust: unknown subcommand '$sub'" >&2
        exit 99
        ;;
esac
