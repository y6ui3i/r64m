# The image `cargo xtask bench` measures in (R081): Alpine 3.22.8 with GCC 14.2.0, as
# tools/images/gcc, and NetHack 3.6.7's source made ready to build as in R030, with
# one of its files preprocessed for timing one compilation alone.
#
# The source is not fetched here: xtask copies it into the build's context from where
# it is kept (R64M_NETHACK_SRC), and the sum below must be its.
FROM alpine:3.22.8@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add -q build-base=0.5-r3 gcc=14.2.0-r6 g++=14.2.0-r6 musl-dev=1.2.5-r13 binutils=2.44-r3 make=4.4.1-r3 ncurses-dev
COPY nethack-367-src.tgz /src/
RUN echo "98cf67df6debf9668a61745aa84c09bcab362e5d33f5b944ec5155d44d2aacb2  /src/nethack-367-src.tgz" | sha256sum -c -
# The Linux hints with -O2 and the C of the source's time. The test for the Linux text
# console is taken out of unixtty.c: it needs a header of the kernel's that is not in
# this image. Alpine's fortified headers are turned off (the curses interface does not
# compile with them), and the Guidebook is said to be made already (its making needs
# nroff).
RUN cd /src && tar xzf nethack-367-src.tgz && rm nethack-367-src.tgz && cd NetHack-3.6.7 \
 && sed -e 's|^CFLAGS=-g -O |CFLAGS=-O2 -std=gnu17 -w -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0 |' -e 's|^LFLAGS=-rdynamic|LFLAGS=|' sys/unix/hints/linux > sys/unix/hints/r64m \
 && (cd sys/unix && sh setup.sh hints/r64m) \
 && cp sys/share/dgn_yacc.c sys/share/lev_yacc.c sys/share/dgn_lex.c sys/share/lev_lex.c util/ \
 && cp sys/share/dgn_comp.h sys/share/lev_comp.h include/ \
 && sed -i -e '/#include <sys\/vt.h>/d' -e 's/struct vt_mode vtm;//' -e 's/ioctl(0, VT_GETMODE, &vtm) >= 0/0/' sys/share/unixtty.c \
 && touch doc/Guidebook
# One file preprocessed as the build compiles it (its headers made by a build that is
# then thrown away), for `cc1` alone.
RUN cp -a /src/NetHack-3.6.7 /tmp/b && cd /tmp/b && make all > /dev/null 2>&1 \
 && cd src && rm -f apply.o && cmd=$(make -n apply.o | grep -- ' -c ' | tail -1) \
 && mkdir /bench && sh -c "$(echo "$cmd" | sed -e 's/ -c / -E /' -e 's/ -o [^ ]*//') -o /bench/apply.i" \
 && cd / && rm -rf /tmp/b && wc -l < /bench/apply.i
CMD ["/bin/sh"]
