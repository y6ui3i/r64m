# The measure

`cargo xtask bench` runs a few pieces of real work under r64m, a number of times each,
and says the least, the median and the most of their times. With `--against FILE` it
runs another r64m (an arm64 Linux build of it) in turn with this tree's, round by
round, so that the two meet the same load; then it says the ratio of their medians.

```sh
cargo xtask bench                                  # every work, this tree's r64m
cargo xtask bench --against old/r64m compile build # two works, this tree's against another
cargo xtask bench --rounds 3 start                 # fewer or more rounds than the work's own
```

| work | what is timed | image | rounds |
|---|---|---|---|
| `compile` | GCC 14.2's `cc1 -O2` on one of NetHack's files, preprocessed | `r64m-bench:amd64` | 8 |
| `hello` | a file of one line compiled and linked by `gcc -O2`: several short processes | `r64m-bench:amd64` | 10 |
| `build` | NetHack 3.6.7 built by `make all` with one job | `r64m-bench:amd64` | 3 |
| `start` | `python3 -c pass` | Python 3.13 by its digest | 10 |

How it is timed. Each work runs in its x86-64 image, started by `docker run`; inside
the container a program of the host's (xtask itself, built for arm64 Linux) starts
r64m for the work and reads the clock around it. Docker's starting a container, and
any shell of the guest's around the work, are not in the time.

What it needs:

- Docker, and the targets `aarch64-unknown-linux-musl` for Rust.
- NetHack 3.6.7's source (`nethack-367-src.tgz`, sha256
  `98cf67df6debf9668a61745aa84c09bcab362e5d33f5b944ec5155d44d2aacb2`, from
  nethack.org) at `target/bench/nethack-367-src.tgz` or where `R64M_NETHACK_SRC`
  says. xtask does not fetch it. The image is made from `tools/images/bench` the first
  time (it builds NetHack once to have one file preprocessed: some minutes).

What it says. A line for each work and each r64m: how many runs, the least, the median
and the most in seconds, the spread ((most − least) / median), and the kernel's load
before and after as the container sees it. Each run is also added to
`target/bench/history.csv` (when, the commit, the host, the work, which r64m, the
numbers).

Reading it. A difference smaller than the spread is not a difference. On the Mac the
load the container sees is the Docker VM's, not the Mac's: other work on the Mac (R080:
its antivirus at more than one core) shows only as a wide spread. The host for numbers
that decide is one that runs nothing else (the Graviton host of R081 onwards).
