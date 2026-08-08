#!/usr/bin/env python3
"""strip-pe-signature.py — normalize Authenticode out of PE binaries (#750).

The Windows counterpart to `codesign --remove-signature` on macOS. It exists
because Windows has no supported command that undoes signing: `signtool remove
/s` deletes the certificate table but leaves behind the PE checksum signtool
rewrote, so a stripped file still differs from the unsigned one in four bytes.
Both fields have to go for the result to be a canonical, recomputable form.

WHY normalize at all: an Authenticode signature carries a mandatory RFC-3161
timestamp from timestamp.acs.microsoft.com. Those bytes differ on every signing
and can never be reproduced, so a payload digest taken over signed bytes is a
hash nobody — including us — can ever recompute. The reproducible-builds
convention is not to reproduce signatures but to EXCLUDE them (F-Droid compares
APKs "apart from the signature"; the Mach-O convention normalizes
LC_CODE_SIGNATURE away). This is that exclusion, for PE.

WHAT is removed, and nothing else:
  1. the certificate table — the appended Authenticode blob, located via data
     directory entry 4 (IMAGE_DIRECTORY_ENTRY_SECURITY), whose "VirtualAddress"
     is a FILE OFFSET for this one entry rather than an RVA,
  2. that directory entry itself (zeroed),
  3. the optional header's CheckSum field (zeroed) — signing recomputes it, so
     leaving it in would make the digest depend on the signature after all.

WHAT this is NOT: a claim to recover the exact pre-signing bytes. signtool pads
the file to an 8-byte boundary before appending the certificate table, so up to
7 zero bytes of alignment padding may remain after truncation. That is fine and
deliberate: the digest's job is to be DETERMINISTIC and INDEPENDENTLY
RECOMPUTABLE from the shipped artifact — anyone holding the public installer
runs this and gets the same number — not to equal a build nobody outside CI
ever had. Stripping trailing zeros to chase byte-identity would be guesswork
that corrupts any binary legitimately ending in zeros.

Usage:
    strip-pe-signature.py strip <path>...    normalize in place; report each change
    strip-pe-signature.py check <path>...    exit 1 if any PE still carries a signature

Paths may be files or directories (walked recursively). Non-PE files are
skipped silently — a bundle tree is mostly resources.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

# Offsets from the PE spec (winnt.h). e_lfanew lives at 0x3C in the DOS header
# and points at the "PE\0\0" signature; the COFF header follows it (20 bytes),
# then the optional header.
E_LFANEW_OFFSET = 0x3C
COFF_HEADER_SIZE = 20
PE_SIGNATURE = b"PE\0\0"

# Within the optional header.
CHECKSUM_OFFSET = 64
# Data directories start after the magic-dependent tail of the optional header.
DATA_DIR_OFFSET = {0x10B: 96, 0x20B: 112}
# IMAGE_DIRECTORY_ENTRY_SECURITY, 8 bytes each (offset, size).
SECURITY_DIR_INDEX = 4
DATA_DIR_ENTRY_SIZE = 8


class NotPE(Exception):
    """The file is not a PE image — skip it rather than fail the run."""


def _locate(data: bytes) -> tuple[int, int]:
    """Return (checksum_offset, security_dir_entry_offset) as absolute file offsets.

    Raises NotPE for anything that is not a well-formed PE image with a security
    data directory. Every bound is checked: this parses attacker-reachable files
    in the sense that a corrupt build output must produce a clean error, never a
    silently wrong digest.
    """
    if len(data) < E_LFANEW_OFFSET + 4 or data[:2] != b"MZ":
        raise NotPE("no MZ header")
    (e_lfanew,) = struct.unpack_from("<I", data, E_LFANEW_OFFSET)
    if e_lfanew + COFF_HEADER_SIZE + 4 > len(data):
        raise NotPE("e_lfanew out of range")
    if data[e_lfanew : e_lfanew + 4] != PE_SIGNATURE:
        raise NotPE("no PE signature")

    opt = e_lfanew + 4 + COFF_HEADER_SIZE
    if opt + 2 > len(data):
        raise NotPE("no optional header")
    (magic,) = struct.unpack_from("<H", data, opt)
    if magic not in DATA_DIR_OFFSET:
        raise NotPE(f"unknown optional header magic 0x{magic:x}")

    dirs = opt + DATA_DIR_OFFSET[magic]
    # NumberOfRvaAndSizes sits immediately before the directories.
    (count,) = struct.unpack_from("<I", data, dirs - 4)
    if count <= SECURITY_DIR_INDEX:
        raise NotPE("no security data directory")
    entry = dirs + SECURITY_DIR_INDEX * DATA_DIR_ENTRY_SIZE
    if entry + DATA_DIR_ENTRY_SIZE > len(data):
        raise NotPE("security data directory out of range")
    return opt + CHECKSUM_OFFSET, entry


def signature_of(path: Path) -> tuple[int, int] | None:
    """(offset, size) of the certificate table, or None if unsigned/not a PE."""
    try:
        data = path.read_bytes()
        _, entry = _locate(data)
    except (NotPE, OSError):
        return None
    offset, size = struct.unpack_from("<II", data, entry)
    return (offset, size) if offset and size else None


def strip(path: Path) -> str | None:
    """Normalize one file in place. Returns a description of what changed, or None."""
    try:
        data = bytearray(path.read_bytes())
        checksum, entry = _locate(bytes(data))
    except NotPE:
        return None

    offset, size = struct.unpack_from("<II", data, entry)
    changed = []

    if offset and size:
        if offset > len(data):
            raise SystemExit(
                f"{path}: certificate table offset {offset} is past EOF ({len(data)}) —"
                " refusing to truncate a file this parser does not understand"
            )
        # The certificate table is always last; anything after it would be lost,
        # so prove there is nothing there before truncating.
        if offset + size < len(data):
            raise SystemExit(
                f"{path}: {len(data) - offset - size} byte(s) follow the certificate"
                " table — this is not the layout signtool produces, refusing to truncate"
            )
        del data[offset:]
        struct.pack_into("<II", data, entry, 0, 0)
        changed.append(f"removed {size}-byte certificate table at {offset}")

    if data[checksum : checksum + 4] != b"\0\0\0\0":
        struct.pack_into("<I", data, checksum, 0)
        changed.append("zeroed PE checksum")

    if not changed:
        return None
    path.write_bytes(bytes(data))
    return f"{path}: " + ", ".join(changed)


def walk(paths: list[str]):
    for raw in paths:
        p = Path(raw)
        if p.is_dir():
            # Sorted so the log reads the same on every run.
            yield from sorted(f for f in p.rglob("*") if f.is_file())
        elif p.is_file():
            yield p
        else:
            raise SystemExit(f"strip-pe-signature: no such path: {raw}")


def main(argv: list[str]) -> int:
    if len(argv) < 3 or argv[1] not in ("strip", "check"):
        print(__doc__, file=sys.stderr)
        return 2
    mode, paths = argv[1], argv[2:]

    if mode == "strip":
        for f in walk(paths):
            note = strip(f)
            if note:
                print(note)
        return 0

    # check — the assert-don't-assume half. A silently failed strip would yield a
    # digest over still-signed bytes: a leaf nobody could ever recompute, which is
    # exactly the failure this path exists to prevent.
    still = [f for f in walk(paths) if signature_of(f)]
    for f in still:
        print(f"strip-pe-signature: STILL SIGNED: {f}", file=sys.stderr)
    return 1 if still else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
