# The image tests/container/{gcc,shell,busybox}.txt were taken in on hal: Alpine
# 3.22.8 with its C compiler. The sums in gcc.txt are of what this compiler makes,
# so its packages are named by version: if Alpine no longer has one, the build
# stops and says so, and the cases are to be taken again on the real machine.
FROM alpine:3.22.8@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add -q build-base=0.5-r3 gcc=14.2.0-r6 g++=14.2.0-r6 musl-dev=1.2.5-r13 binutils=2.44-r3 make=4.4.1-r3
CMD ["/bin/sh"]
