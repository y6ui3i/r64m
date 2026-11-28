# 0021: Translations kept from one process to the next

**Status**: proposed (2026-11-28; the owner: 「翻訳キャッシュのADRから」). Points 2 to 5
are proposed as written. Point 1, where the translations are kept, is put to the
owner as a choice; the proposal is (a), with (c) as a later step.

## Context

Every process starts with no blocks. An `execve` is the host's: the kernel runs r64m
again by `binfmt_misc` for the new program, and the blocks of the process before are
gone. A build runs the same compiler hundreds of times, and each one translates the
same code again.

R079 accounted for one NetHack build, process by process:

| program | processes | time, summed | making blocks | share |
|---|---|---|---|---|
| `cc1` | 137 | 214.9 s | 41.6 s | 19% |
| `as` | 137 | 5.9 s | 1.5 s | 25% |
| `ld` | 6 | 0.4 s | 0.1 s | 27% |

`cc1` translates 1.1 million of its instructions in every process, at 277 ns each.
And until a block is made, its code is run by the interpreter (`HOT` = 4 arrivals,
R080), which is time that a kept block would not cost either. The short works are
furthest behind native (R082: `hello` 6.9 times, `start` 6.4 times, against 2.8 for
one long compilation), and those are almost all starting and translating.

What makes this possible:

- Generated code holds no host address (it reaches the processor's state, the
  guest's memory and the table by registers: `CPU`, `BASE`, `TABLE`). The only
  exception is the counters of `--jit-count`, which is for measuring. A block's code
  can be copied to another place and run there. The joins, the landings of returns
  (`pads`) and the table's size (`lookups`) are written into it after it is made,
  by whoever keeps the blocks, and are known by their offsets in `Built`.
- A program is put at the same addresses every time. r64m does not randomize
  (`PIE_AT`, `INTERP_AT`), so the same `cc1` is at the same guest addresses in every
  process.
- What a block is made from is known exactly: the guest's bytes from its start to
  its end, and the bytes it rests on besides (`Built::seen`, R013). It also depends
  on how it was made: `exact`, `ordered`, the count (R083), counting (R076) and
  r64m's own build.

## Decision

1. **Where the translations are kept: a choice for the owner.**
   - **(a) A directory named by `R64M_CACHE`. Proposed first.** Nothing is kept
     unless the variable is set. In Docker the directory is a volume, given with
     `-v` and `-e`. r64m takes the variable out of the environment the guest sees,
     so the guest finds no difference, and children are given it again by r64m.
     It is simple, it is explicit, and it can be measured. It also keeps
     translations across containers and across builds.
   - **(b) A directory inside the container's own files (`/tmp/.r64m`, say).
     Not proposed.** It would work with nothing given, but the guest would see a
     file it did not make. That goes against 0018 (behave as x86-64).
   - **(c) Memory shared through the process tree. A later step.** The first r64m
     makes a shared region (a `memfd`), and each child inherits it as a descriptor
     the guest is not shown. It works with nothing given, and it lasts exactly as
     long as the build that uses it. The cost is hiding a descriptor: the guest's
     `/proc/self/fd`, `close_range`, and a `dup2` onto its number. It is worth it
     only if (a) shows the gain.

2. **A kept block is used only if the guest's bytes are the same.** The cache is
   keyed by the guest's address. With each block it holds the bytes the block was
   made from (its instructions and its `seen`), and those are compared, byte for
   byte, with the guest's memory before the block is used. A different library at
   the same address, a changed file, or a program that wrote its own code all fail
   the comparison, and the block is made anew as today. Nothing is trusted from a
   file's name, date or inode. Those only choose which file of the cache to read.

3. **What a block is made under is part of its key.** That means `exact`,
   `ordered`, whether the count is kept, and r64m's build (a hash of its own
   executable, taken once). A cache written by another r64m is not read. Under
   `--jit-count`, `--jit-check` and the tests nothing is kept or read.

4. **What is kept and when.**
   - Each process reads, at its start, the file for each code region it maps.
     The file is chosen by the region's file identity and its address.
   - A kept block is used at its first arrival, not after `HOT` arrivals: it costs
     no translation.
   - At its end (and before an `execve`), a process adds the blocks it made that
     the file did not have. It writes a new file and renames it over the old one,
     so a reader never sees half a file, and of two writers the last wins: a lost
     block is made again, never wrong.
   - Joins, landings and the table's size are not kept. They are written again
     after the block is copied in, as for a block just made.
   - Only code from pages the guest may not write is kept (0016). A JIT inside the
     guest is never cached.

5. **A cache is code that r64m runs.** Whoever can write to it can make r64m run
   anything as the guest's user. That is no more than writing the program itself,
   but it must not be less guarded than the program is:
   - r64m uses the directory only if it is owned by the user r64m runs as and is
     not writable by others. Otherwise it is ignored, and r64m says so once.
   - A file that is short, or not of this format and build, is ignored.
   - This keeps 0008 as it is: r64m is not a boundary of security, and this does
     not make it one.

## Consequences

- **The gain to be measured, as its own card, before any of (c):** the NetHack
  build, `hello` and `start` on Graviton, with the cache cold and warm. The bound
  is R079's: making blocks is 19% of `cc1`, with the interpreted time before `HOT`
  on top.
- **The first process pays a little more.** It writes its blocks out at its end.
- **The cache grows with what is run.** `cc1`'s blocks are some tens of
  megabytes. Clearing it is deleting the directory. A limit on its size, if
  needed, is a later card.
- **A wrong block is ruled out by point 2,** not by care. The tests add a case
  that changes a library's bytes at the same address between two processes, and
  one with a cache written by another build.
- **The checker and the measure stay honest.** `--jit-check` and `--jit-count`
  never read the cache, so what they check and count is translation as it is.

## What else was weighed

- **Trusting the file's identity (path, size, date, inode) without comparing
  bytes.** It is cheaper, but a file replaced in place with the same date, or a
  different library at the same address, would run the wrong code. The comparison
  is a few bytes for each block used, made once.
- **Hashing each code region at its start.** That is one key for the whole
  region, but it reads the whole of `cc1`'s code (some tens of megabytes, not
  measured) in every process, which could cost more than a short process gains.
- **Translating faster instead (R080's other half).** That is worth doing anyway,
  but it cannot reach zero, and a kept block also saves the interpreted arrivals
  before `HOT`.
