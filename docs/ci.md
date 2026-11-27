# The CI

`.github/workflows/ci.yml`, on GitHub Actions. Every step is a `cargo xtask`
command (`xtask/src/main.rs`), so what the CI runs can be run by hand:

| command | what it runs | needs |
|---|---|---|
| `cargo xtask test` | the unit tests, the primitives, the traces | g64m's checkout beside this one (or `R64M_G64M`), `nasm` |
| `cargo xtask guest` | the guest's own programs (signals) | Linux |
| `cargo xtask images build` | the two images that are built, not pulled (`tools/images/`) | Docker |
| `cargo xtask container [--ways jit] [--jobs N] [SUITE ...]` | programs of x86-64 images, held to what hal printed | Docker, the two targets of `rustup` |
| `cargo xtask ci` | all of these that the host can run | |

## What runs when

| when | where | what |
|---|---|---|
| every pull request; a push to `main` (not one of documents only) | arm64 Linux | tests, guest, and the container cases by the JIT |
| Saturdays 18:17 UTC, and by hand ("Run workflow") | arm64 Linux | the same with the container cases three ways (interpreter, JIT, JIT checked) |
| | macOS (arm64) | `cargo xtask test` |

`main` is protected (since 2026-11-27): it changes only by a pull request, and a
pull request is merged only when the job `linux` has passed on it; no push straight
to it, no forced push, no deletion, for the owner too. That is why a pull request of
documents only is run as well.

The images of the container cases are Docker Hub's by digest (the very images the
cases were taken in on hal; `SUITES` in `xtask/src/main.rs`), but two that are
built: Alpine with its compiler (packages named by version: the sums in
`tests/container/gcc.txt` are of what that compiler makes) and the goal's (Python
with FastAPI and uvicorn, Debian's nginx). Those two are built on an x86-64 runner,
where their own programs run, kept in the Actions cache by the Dockerfiles' hash,
and loaded on the arm64 runner. g64m is checked out at a fixed commit.

The expected lines of every case are the real machine's and stay so: the CI runs
r64m against them; it does not make them.

## How long a pull request waits

Measured on 2026-11-27 (R061), the job `linux`: 3 minutes 16 seconds. Setting up
(checkouts, `nasm`, `rustup`) 30 seconds; the unit tests, primitives and traces 70,
mostly their build; the container cases 82, four suites at once (135 one at a time);
loading the two built images 10. A container case that does not end fails after 300
seconds with every thread's call (R060); the job itself has 120 minutes.

## What it cost while the repository was private

The repository is public since 2026-11-27, and standard runners cost nothing for
it. What follows is what was measured and reckoned before, kept for the numbers.

GitHub Free includes 2,000 minutes a month; arm64 Linux counts 1 for 1 (about
$0.005 a minute beyond that), x86-64 Linux about 1.2, macOS about 12.

- A push, measured on 2026-11-26 (the second run; the first found that the
  container cases must not be run on the runner itself): 7 seconds of x86-64 (the
  image cache looked up; 53 seconds when the two images are built) and 5 minutes
  35 seconds of arm64 Linux, of which the tests and their builds are 2 minutes and
  the container cases 2 minutes 46 seconds. Counted as 7 minutes: some 280 pushes
  a month fit.
- The weekly run, not yet run, an estimate: perhaps 60 to 90 minutes of arm64
  Linux and 4 of macOS (counted as about 50): about 600 minutes a month.

Beyond the included minutes nothing is charged unless a budget for Actions was set
in the account; the jobs then do not start. Once the repository is public, standard
runners cost nothing, and macOS and the three ways can go on every push.

## Not in it yet

`cargo fmt --check` and `cargo clippy` (the components are not installed where r64m
is written; whether the tree passes is not known). The races against QEMU: timings
on shared runners say little. The cases taken again on an x86-64 runner as a
second real machine.
