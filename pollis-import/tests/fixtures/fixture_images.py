"""Tiny valid PNG and GIF writers for import fixtures, so no image tooling is needed."""

import struct
import zlib


def png(path, w, h, fn):
    raw = b""
    for y in range(h):
        raw += b"\x00" + b"".join(bytes(fn(x, y)) for x in range(w))

    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    data = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    open(path, "wb").write(data)


def lzw(indices, min_code=2):
    """GIF LZW with a clear code before every pixel: fixed code size, valid, tiny images only."""
    clear = 1 << min_code
    eoi = clear + 1
    size = min_code + 1
    out = []
    bits = 0
    nbits = 0

    def emit(code):
        nonlocal bits, nbits
        bits |= code << nbits
        nbits += size
        while nbits >= 8:
            out.append(bits & 0xFF)
            bits >>= 8
            nbits -= 8

    for i in indices:
        emit(clear)
        emit(i)
    emit(eoi)
    if nbits:
        out.append(bits & 0xFF)
    b = bytes(out)
    res = b""
    for k in range(0, len(b), 255):
        res += bytes([len(b[k : k + 255])]) + b[k : k + 255]
    return bytes([min_code]) + res + b"\x00"


def gif(path, w, h, frames, palette):
    pal = b"".join(bytes(c) for c in palette) + b"\x00" * (3 * (4 - len(palette)))
    d = b"GIF89a" + struct.pack("<HHBBB", w, h, 0xF1, 0, 0) + pal
    d += b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00"
    for f in frames:
        d += b"\x21\xF9\x04\x00" + struct.pack("<H", 30) + b"\x00\x00"
        d += b"\x2C" + struct.pack("<HHHHB", 0, 0, w, h, 0) + lzw(f)
    d += b"\x3B"
    open(path, "wb").write(d)
