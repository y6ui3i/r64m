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

## `main` changes by pull request, merged by the owner

The work is on the branch `port`: commits and pushes go there. `main` is what a
visitor sees, and it is protected on GitHub (since 2026-11-27, when the repository was
made public): no push straight to it, no forced push, no deletion; it changes only by a
pull request on which the CI's job `linux` has passed. A pull request from `port` to
`main` is opened when a step is committed and verified; merging it is the owner's to do
or to say.

(Until 2026-11-26 `main` held the skeleton alone and waited for a version that works:
[ADR 0002](docs/adr/0002-main-waits-for-a-version-that-works.md). What "works" came to
mean is [ADR 0020](docs/adr/0020-what-done-means.md). It was brought up to `port` by the
owner's word on 2026-11-26 and 2026-11-27, before the protection.)
