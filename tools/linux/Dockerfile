# An arm64 Linux with a C compiler, to build g64m in (R001): g64m is the reference r64m is
# held to, and it must run where r64m runs. Docker on an Apple silicon Mac is arm64 Linux.
#
#   docker build -t g64m-arm64-build tools/linux
FROM alpine:3.24
RUN apk add --no-cache gcc musl-dev binutils
