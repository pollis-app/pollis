#!/usr/bin/env python3
"""A local HTTP CONNECT proxy that can hold traffic back on demand (#1636 e2e).

    python3 mobile/scripts/slow-proxy.py <port> <switch-file> [delay-ms]

The Android emulator, started with `-http-proxy http://127.0.0.1:<port>`, sends
every guest TCP connection through it. While <switch-file> exists, each chunk in
either direction waits [delay-ms] (default 3000) before it is passed on: every
request the app makes becomes slow, but nothing is cut, so a send stays in flight
instead of failing. Remove the file and traffic flows at full speed again,
including on connections that are already open.

The emulator console's own `network delay` does not shape traffic on current
emulator builds, which is why this exists. Test tooling only; it never ships.
"""

import asyncio
import os
import sys

PORT = int(sys.argv[1])
SWITCH = sys.argv[2]
DELAY = (int(sys.argv[3]) if len(sys.argv) > 3 else 3000) / 1000.0


async def pump(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while True:
            chunk = await reader.read(65536)
            if not chunk:
                break
            if os.path.exists(SWITCH):
                await asyncio.sleep(DELAY)
            writer.write(chunk)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


async def handle(client_r: asyncio.StreamReader, client_w: asyncio.StreamWriter) -> None:
    try:
        head = await client_r.readuntil(b"\r\n\r\n")
    except Exception:
        client_w.close()
        return
    first, _, rest = head.partition(b"\r\n")
    parts = first.decode("latin-1").split()
    if len(parts) < 3:
        client_w.close()
        return
    method, target, version = parts[0], parts[1], parts[2]
    if method.upper() == "CONNECT":
        host, _, port = target.rpartition(":")
        forward = b""
    elif target.startswith("http://"):
        # A plain-HTTP request with an absolute URI: forward it with the path only.
        hostport, _, path = target[len("http://"):].partition("/")
        host, _, port = hostport.partition(":")
        port = port or "80"
        forward = f"{method} /{path} {version}\r\n".encode("latin-1") + rest
    else:
        client_w.write(b"HTTP/1.1 400 Bad Request\r\n\r\n")
        client_w.close()
        return
    try:
        up_r, up_w = await asyncio.open_connection(host, int(port))
    except Exception:
        client_w.write(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
        client_w.close()
        return
    if forward:
        up_w.write(forward)
        await up_w.drain()
    else:
        client_w.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        await client_w.drain()
    await asyncio.gather(pump(client_r, up_w), pump(up_r, client_w))


async def main() -> None:
    server = await asyncio.start_server(handle, "127.0.0.1", PORT)
    print(f"slow-proxy on 127.0.0.1:{PORT}, switch {SWITCH}, delay {DELAY}s", flush=True)
    async with server:
        await server.serve_forever()


asyncio.run(main())
