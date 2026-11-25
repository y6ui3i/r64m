# 0002: `main` is not updated until r64m does what g64m does

**Status**: accepted (2026-11-24, the owner); its bar changed by [0018](0018-one-thing-well.md)
(2026-11-25): the container's goal (0011), not g64m's list

## Context

The owner wants the repository kept clean and its first version to be one that works:
not a history of halves on `main`.

## Decision

`main` holds the skeleton and nothing else until r64m does what the 64-bit g64m does
today: runs programs without a kernel, boots Linux, builds NetHack in the guest, with
the JIT. Then the work lands on `main` once. Until then everything is on the branch
`port`: commits and pushes go there, no pull request is opened against `main`, nothing
is merged into it.

## Consequences

- `port` is long-lived. What would be pull requests in g64m are commits on it.
- The bar for landing is g64m's own gate, met by r64m.
