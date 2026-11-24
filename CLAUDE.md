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

## `main` is not updated until r64m does what g64m does

Decided by the owner on 2026-11-24: the repository is to stay clean, and its first version
is to be one that works. `main` holds the skeleton and nothing else until r64m is at
g64m's level; then the port lands on it once. Until then all work is on the branch
`port`: commits and pushes go there, no pull request is opened against `main`, and nothing
is merged into it.
