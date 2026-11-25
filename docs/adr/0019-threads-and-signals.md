# 0019: A thread is the host's thread; a signal is the host's signal, delivered between blocks

**Status**: accepted (2026-11-25, the owner: the three points, A, B and C, as proposed).
Point D, on the order of memory between threads, was missed in the proposal and added
the same day when the work began; accepted then too (the owner: 「X86_64 に合わせて」:
as x86-64 has it).

## Context

The goal's programs (0011) start threads and take signals. R032 counted: Python
starts none to print a line but its servers do (`clone` with shared memory, `futex`);
a server waits in `epoll` and on sockets; a signal reaches a handler when a timer
fires or the server is told to stop. r64m refuses `clone` with shared memory and
keeps a handler without ever calling it (0017, "Not yet").

By the first principle (0018) the question for each is what the host's kernel does
already. It schedules threads, it keeps futexes, it chooses which thread a signal
goes to, it restarts a call that a signal cut short. What only r64m can do is what
is x86-64's: each thread's registers, and the frame a handler is entered with.

## Decision

1. **A: a guest thread is a host thread.** `clone` with shared memory starts one
   thread of the host's (by the host's C library, so that r64m's own thread-local
   state is whole), which runs the guest from the registers `clone` gave it: its own
   CPU state, the stack and the FS base the guest chose. The guest's thread id is
   the host's. `futex` is the host's (R032). When the thread ends, r64m clears the
   word the guest asked to have cleared and wakes who waits on it, as the kernel
   does. `exit` ends the thread; `exit_group` the process. `clone3` is refused: the
   C libraries then use `clone`.
   - **An instruction with the LOCK prefix, and an exchange with memory, is atomic**:
     no other thread's store can fall between its read and its write. (Until now
     the interpreter read, worked, and wrote, which is the same thing with one
     thread and is not with two: every mutex of a C library is such an instruction.)
     The interpreter writes with the host's compare-and-swap what it read, and runs
     the instruction again if another thread got there first. These instructions
     are the interpreter's, not blocks'.
   - **Each thread has its own blocks.** Generated code and the table of blocks are
     a thread's own, as its registers are: nothing is locked to make a block, to
     join two, or to run one. The price is that code two threads run is translated
     twice and held twice. Shared translations wait until that is counted.
   - **The guest's memory is one, and the list of its regions is shared**: changed
     under a lock, with a number that goes up at each change; a thread looks at the
     number before it next asks the list, and takes the list anew if it moved.
   - **What is given up.** (i) Code changed by one thread is seen by another only
     when that one next leaves generated code (point 2's test makes that soon, not
     at once). x86 says a program that changes code another thread is running must
     see to the order itself. (ii) A thread that reads memory another thread unmaps
     at the same moment, in an instruction the interpreter runs, can fault inside
     r64m and end the process, where x86 would give that thread a fault. A program
     that does this is wrong on x86 too.

2. **B: a signal is taken by the host's handler and delivered between blocks.**
   - When the guest gives a signal a handler, r64m gives the host's signal its own
     handler (with the guest's flags that are the kernel's business: restart of
     calls, the mask while it runs). It notes the signal for the thread it arrived
     on and sets a word in that thread's state.
   - **Every block begins by looking at that word** (a load and a branch): if it is
     set, generated code is left at once, at an instruction's start, with every
     register in its place. Between blocks and between the interpreter's
     instructions r64m then enters the guest's handler as Linux on x86-64 does: the
     frame on the guest's stack (or its alternate stack) with what the signal was
     and every register, floating point's too; the return address the guest's
     restorer; `rt_sigreturn` puts the registers back from the frame. What that look
     costs is measured on the primitives first (a card): if it shows, only blocks a
     jump backward arrives at look.
   - **A fault of the guest's own** (memory it may not touch, a division by nothing,
     what is not an instruction) is delivered the same way if it has a handler, at
     the instruction that faulted, with the address; if it has none, the process
     ends as by that signal, not with r64m's own message and code.
   - A signal with no handler is the kernel's as now (0017): ignored, or the end.

3. **C: the order**, by what the goal's programs meet, each a card held to what the
   real machine prints:
   1. threads: Python's four threads; then redis-server's start;
   2. sockets and `epoll`: handed on, with `epoll`'s structure (packed on x86-64,
      not on arm64) and a message's header written over; Python's HTTP server
      answering a request;
   3. signals delivered: a timer's (redis), a child's end, a server told to stop;
   4. the goal: FastAPI behind nginx, over TLS. (What it needs fetched is asked for
      then.)

4. **D: between threads, memory is ordered as x86 orders it.** x86
   promises more than arm64 does: a thread's stores reach the others in the order
   it made them, and its loads see them in order (TSO). A compiled x86-64 program
   counts on it wherever it shares memory without a LOCK: one thread writes a value
   and then a flag, with two plain stores, and another reads the flag and then the
   value. On arm64 two plain stores may reach another core in either order, so a
   block that is a plain load or store for each of the guest's would, rarely and
   not repeatably, give the second thread the flag without the value. (Rosetta
   avoids it by a mode of Apple's own processors that orders memory as x86 does;
   QEMU by a barrier at each access.)
   - Proposed: **while a process has one thread, blocks are as they are; from its
     second thread on, blocks are made with the host's ordered load and store**
     (`ldapr` and `stlr`: a load that sees what was released, a store that
     releases), and blocks made before are dropped once. A program with one thread,
     which is every program of the build raced in R030, pays nothing. What a
     program with threads pays is measured (a card) before anything else is tried.
   - The other ways: plain loads and stores always (fast, and wrong in a way no
     test of r64m's would show and a real server sooner or later would); the
     processor's own x86 order where the host has it (Apple's, and only if the
     kernel lets a process ask: to be looked at; a Raspberry Pi has none).
   - (Threads were built first with plain loads and stores, R033; ordered in R034.
     As built in R035: where the kernel gives a thread the processor's own x86 order,
     every thread of the guest's asks for it and blocks stay plain; the ordered
     blocks are for hosts that have no such thing.)

## Not yet

- (As built, R037 and R039.) The look is one instruction, a test of the top bit of
  the count of instructions, which the host's handler sets in a thread it
  interrupts in generated code. It costs a loop of twenty instructions one more: 1
  to 4% of the tightest. A signal that arrives
  between the look at a call's start and the kernel taking the call waits out the
  call: a few instructions wide, owed.

- Shared translations between threads; a robust futex list (`set_robust_list` is
  refused, which the C libraries take); `rseq`; `clone3`.
- A signal delivered inside a call that waits, where the guest asked for no restart,
  is the kernel's EINTR; one delivered while a block runs waits for the block's next
  start. Real-time order among several pending signals is not kept beyond the
  kernel's own.
- MMX (R032).

## Consequences

- `Cpu`, the JIT and the interpreter's caches are per thread already in kind; `Mem`
  becomes a thread's view of a shared list.
- Each block is two host instructions longer (or loop heads only, by the card).
- The JIT's known limit, flags that are stale at a fault where nothing reads them
  (R013), stays right under delivery: a handler's frame holds them, `rt_sigreturn`
  puts them back, and from that instruction on nothing reads them before they are
  written.
- By 0018: no scheduler, no signal queue of r64m's own beyond "this thread has
  these pending", no thread of r64m's that is not a guest's.
