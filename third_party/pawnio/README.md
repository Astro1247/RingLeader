# PawnIO modules

`IntelMSR.bin` and `AMDFamily17.bin` are unmodified signed builds from
[PawnIO.Modules](https://github.com/namazso/PawnIO.Modules) release 0.2.11,
licensed under the GNU LGPL v2.1 (see `COPYING`). Source: `IntelMSR.p`, `AMDFamily17.p` in that repository.

RingLeader hands them to the [PawnIO](https://pawnio.eu) driver at runtime to read the CPU package
temperature on Windows. They are data loaded by the driver; RingLeader does not link against them.
To use a different build, replace the files and rebuild.
