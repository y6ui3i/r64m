# 0020: What done means: it runs, it is fast, it is compatible

**Status**: accepted (2026-11-26, the owner: 「ゴールの定義がないなぁ」, and of the
definition proposed in answer: 「定義の案で一旦 main に入れようか」; then: 「あといくつかの
コンテナを試して、Error 無く動くところまで(互換の担保)を取れるといいなぁ」 and
「コンテナネットワークのテストもできるといいかな。そこで、負荷を掛けて NW が足かせになるなら
そこも見直そうか」)

## Context

0011 says what r64m is for: an HTTPS request through nginx to FastAPI in an x86-64
container. 0018 says twice QEMU's speed is the aim. Neither says by which measure, on
which work, or when it is enough; and so each number under 2 became one more
experiment (R042 to R046), with no line to stop at. The request first completed in
R041.

## Decision

r64m is done, for this goal, when the four points below hold. Each is a thing that
can be run and read.

1. **It runs.** In the ordinary images, unchanged (the official nginx; a Python
   image with FastAPI installed in it by its own tools), started by `docker run`
   with nothing naming r64m on the command line, the request of 0011 completes,
   and what it prints is what the real machine prints.
2. **It is fast.** By one client's clock, the same client for both, each of three is
   at least twice QEMU's (user mode, the one the host's Docker runs x86-64 images
   with): from the container's start to the first answer; a request on a new
   connection; a request on a connection kept open. And the build of NetHack in a
   container (R030) is at least twice QEMU's.
3. **It is compatible.** A list of ordinary images, each started, asked to do one
   thing that is its work, and ended, runs without an error and prints what the
   real machine prints; each is kept as a test. The first list: nginx, Node,
   PostgreSQL, a JVM, memcached, Go, beside Python and redis which are there.
   And containers that talk to one another over Docker's network, under load: if
   the network is what holds the work back, how r64m meets it is looked at again.
4. **The primitives are a tool, not the bar.** They are raced to find what a change
   slowed. One that is behind QEMU, or made slower for the goal's sake, is written
   down with its reason.

What a process of r64m costs in memory (the tables each one has, 0019 and R-numbers
to come) is known to be more than it should be for many containers; it is measured
with point 3 and is not yet a point of its own.

**`main` is updated now**, once, with r64m as it stands, by the owner's word; and
again when the four hold. Until then the work goes on on `port` as before (0018,
point 5).

## Where it stands on the day

| | | |
|---|---|---|
| 1. runs | the request completes and matches the real machine (R041) | not yet in unchanged images, nor by `docker run` alone |
| 2. fast | the start 2.98, a new connection 2.34, the build 2.9 (R031) | a kept connection 1.84 (R046) |
| 3. compatible | Python, redis, GCC, busybox | the list, and the network, not begun |
| 4. primitives | 22 of 23 past QEMU at R039 | p_ldbl behind; p_recursion slower since R046 |

## Consequences

- An experiment is for one of the four points, and says which.
- A number under 2 that is not one of point 2's four is not by itself a reason for
  an experiment.
- Registering r64m as the runner of x86-64 programs in the host's Docker (point 1)
  changes the whole Docker VM and is still the owner's to say when.

**Where it stands after R047 to R050 (2026-11-26, later)**: point 1 holds (R050: the
ordinary images unchanged, two containers, `docker run` alone, r64m registered in
QEMU's place by the owner's word). Point 3: eight images run as the real machine
has them; under load the network holds the work back on the real machine as much
as under r64m. Point 2 as before (a kept connection 1.84). Memory: a guest thread
costs about 18 MB of r64m's own, which is the next thing to decide.
(R051: that was read wrongly. A thread that ended gave nothing back; fixed. The
goal's server holds 92 MB under load where the real machine holds 48, and stays
there. Node's workers failing one run in ten was `clone` writing a thread's id
late: fixed in R052.)

**Where it stands after R051 to R067 (2026-11-27)**: the repository is public, and
r64m is put into Docker and taken out by one command each (`cargo xtask install`,
`uninstall`). Point 1 holds. Point 2 as before, raced again (R067): the start 3.05,
a new connection 2.42, the build 2.9, a kept connection 1.92, under the aim. Point
3: eleven images, about a hundred cases in the CI by the JIT on every pull request
and by three ways weekly; threads and `fork` held to the real machine with musl
and with glibc (R065); `apt-get install` runs. What load found and cases had not
(R052, R055 to R057, R064, R066) is fixed and each has its case. Known not to
work: robust mutexes.
