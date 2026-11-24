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
