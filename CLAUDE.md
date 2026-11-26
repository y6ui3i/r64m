# r64m

The Rust port of g64m (https://github.com/y6ui3i/g64m, a checkout is usually beside this
one at `../g64m`).

Everything kept in this repository is written in English: source comments, documentation,
the experiment ledger, commit messages, branch names, and pull request titles and
descriptions.

Conversation with the owner stays in Japanese.

The real CPU is the oracle for behaviour (expected output taken on an x86 host under KVM
or by ptrace, never from another emulator's own emulation). g64m is the oracle for what
r64m is to do and how fast.

An experiment gets a card in `docs/experiments.md` before it runs (hypothesis, what
changed, the next move for each result, the cut-off) and its result in the same place,
whether it worked or not.

## `main` is updated by the owner's word

The work is on the branch `port`: commits and pushes go there. `main` is what a
visitor sees: it is brought up to `port` when the owner says so, by a fast-forward, and
not otherwise (first on 2026-11-26, with ADR 0020; again on 2026-11-27, before the
repository was made public). No pull request is opened against `main` and nothing is
merged into it without that word.

(Until 2026-11-26 `main` held the skeleton alone and waited for a version that works:
[ADR 0002](docs/adr/0002-main-waits-for-a-version-that-works.md). What "works" came to
mean is [ADR 0020](docs/adr/0020-what-done-means.md).)
