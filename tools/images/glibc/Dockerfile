# The image tests/container/threads.txt is run in beside Alpine's: Debian 12 with
# its C compiler and the GNU C library, so that the same programs of threads and
# of fork are held to the real machine with glibc as with musl (R065). The cases
# compare what the programs print, not what the compiler makes, so the packages
# are Debian 12's as they stand on the day the image is built.
FROM debian:bookworm-slim@sha256:e67eeead736ff7bbe258ef088bff683635c561b1fcfefad0806c1df4ab49ce0e
RUN apt-get update -q && apt-get install -y -q --no-install-recommends gcc libc6-dev && rm -rf /var/lib/apt/lists/*
CMD ["/bin/sh"]
