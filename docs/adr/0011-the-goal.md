# 0011: The goal for 64 bits: HTTPS through nginx to FastAPI, in an x86-64 container

**Status**: accepted (2026-11-24, the owner)

## Context

"Does what g64m does" (0002) says when `main` may be updated. It does not say what r64m
is for. g64m's own yardsticks were a boot and a build; what an x86-64 emulator on an
arm64 host is asked for in practice is to run a container image that exists only for
x86-64.

## Decision

The goal is this, on an arm64 host:

> In an x86-64 container, nginx takes a request over TLS and passes it to a Python
> process serving FastAPI, and the answer `Hello, world` comes back to the client.

Both processes are x86-64 programs run by r64m without a guest kernel. The images are
the ordinary ones, unchanged.

## What it asks of r64m, beyond a static program that prints

Listed so that the order of work can be read from it; none of it is decided in detail
here.

- **The dynamic linker**: `ld-linux-x86-64.so.2` or musl's, loaded by r64m beside the
  program (0005), then left to do its own work: `mmap` of files, `mprotect`,
  thread-local storage (`arch_prctl`).
- **More than one process**: nginx is a master and workers (`fork`), and the two
  servers are two programs. With 0008 a guest's memory is the host's, so `fork` can be
  the host's `fork` and `execve` a new r64m.
- **Threads**: `clone`, `futex`, per-thread state. Python and uvicorn start some even
  for one request.
- **Signals** given to the guest (0008 already needs SIGSEGV): SIGCHLD, SIGTERM, the
  handlers nginx installs, `sigreturn`.
- **Sockets and waiting**: `socket`, `bind`, `listen`, `accept4`, `epoll`, `sendfile`,
  `eventfd`, non-blocking descriptors. Most pass to the host's own calls on Linux; on
  macOS there is no `epoll`, which is one more reason the host r64m is made for is
  Linux (0008, point B).
- **Files of the image**: paths resolved inside the image's root, `openat`, `stat`,
  `getdents64`, `/proc/self/*`, `/dev/urandom`.
- **Time**: the vDSO or the calls it stands for (`clock_gettime`).
- **The instructions TLS uses**: OpenSSL chooses its code by CPUID. What r64m says it
  has (AES-NI, PCLMULQDQ, SSE4, AVX, SHA) is what it must then do right, and each is
  held to the real CPU as every instruction is.
- **Self-check**: the request is made by a client outside the container (`curl` on the
  host), and the test is the status, the body and that the handshake completed.

## Consequences

- Programs without a kernel are the main line of r64m, and the first to be made fast.
  The machine (a kernel booted) is still wanted (0002 as it stands), but it is no
  longer the measure.
- The host's kernel does the guest kernel's work, so arm64 Linux is where the goal is
  met. macOS stays the place of development for what does not need Linux's calls.
- How the container is given to r64m (a root directory unpacked, r64m registered as
  the runner of x86-64 programs inside Docker, or something else) is a decision of its
  own, to be made when a dynamically linked program first runs.
- Whether 0002's bar for `main` changes with this goal is the owner's to say; until
  then it stands as written.
