# 0007: The simple form first; what g64m added is added again only when counted

**Status**: accepted (2026-11-24, the owner)

## Context

Much of what is involved in g64m was added behind something else: blocks lived in the
decode cache's entries, then got a table of their own, then a way back to an entry, then
the exits that had waited (E166, E180); a jump to another page had a look-up, then a
table before it, then a cell before that (E134, E165, E178); forty switches to find a
wrong translation by. Each was right for where g64m stood. r64m does not stand there.

Some of what g64m added did make it faster and has no simpler form (a block made at the
eighth time an address comes with no entry, E167). Simple and as fast as g64m will not
always be the same thing.

## Decision

A part of r64m is made in its simplest form and measured. If that is not enough, what is
missing is counted before anything is added, the least that mends it is added, and the
ledger says why. Nothing is brought from g64m because g64m has it.

## Consequences

- The bar is still g64m's speed (0002): where the simple form misses it, the form grows.
- A decision that adds a mechanism names the experiment that showed the need.
