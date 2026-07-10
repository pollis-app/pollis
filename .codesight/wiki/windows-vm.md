# Windows Dev VM (scripts/windows-vm.sh)

Local Windows testing without the CI round-trip: a real Windows 11 ARM64 VM running inside a Docker container ([dockur/windows-arm](https://github.com/dockur/windows-arm)), provisioned with the full Tauri toolchain on first boot. Everything is driven by a single script — `scripts/windows-vm.sh` — and all state (compose file, provisioning scripts, Windows disk) lives outside the repo in `~/.pollis/windows-vm/` plus a docker volume.

## How it works

```
macOS (Apple Silicon M3+, macOS 15+)
  └─ colima VM (vz + nested virtualization → exposes /dev/kvm)
       └─ dockurr/windows-arm container (QEMU/KVM)
            └─ Windows 11 Pro ARM64
```

Docker Desktop **cannot** run this — it doesn't expose KVM on macOS. Colima with nested virtualization is the required path, and only on M3-or-newer chips with macOS 15+.

The generated OEM scripts run automatically during Windows setup and install: VS 2022 Build Tools (MSVC ARM64 + x64 cross tools + Win11 SDK), Git, Node 22, rustup (`aarch64-pc-windows-msvc` host), pnpm, plus the `LongPathsEnabled` registry fix. WebView2 ships with Windows 11. Provisioning logs land in `C:\OEM\*.log` inside the guest.

## Usage

```bash
scripts/windows-vm.sh up        # installs colima if needed, boots everything
scripts/windows-vm.sh stop      # clean ACPI shutdown of Windows
scripts/windows-vm.sh start     # boot it again (fast — state persists)
scripts/windows-vm.sh suspend   # stop Windows AND colima (frees the 12 GB RAM)
scripts/windows-vm.sh down      # remove container, keep Windows disk
scripts/windows-vm.sh down --wipe   # destroy the Windows disk entirely
scripts/windows-vm.sh logs      # follow container logs
scripts/windows-vm.sh status    # colima + container state
```

First `up` downloads the ~5 GB Windows ISO and runs an unattended install (~15–30 min, one time). Watch it at `http://localhost:8006`.

- **Console:** browser at `http://localhost:8006` (noVNC).
- **RDP (nicer):** the Windows App from the Mac App Store → `localhost:3389`, user `dev`, password `pollis`.
- **Files:** `~/.pollis/windows-vm/shared/` appears in the guest as `\\host.lan\Data`. Use it for dropping files only — **clone the repo inside Windows** and build there; building over the network share is painfully slow.

## Caveats

- The guest is Windows 11 **ARM64**: `cargo`/`pnpm dev` build `aarch64-pc-windows-msvc` natively, which covers WebView2 behavior, Windows API paths, installer flows, keystore, etc. x64 binaries run under Windows' built-in emulation, but the shipped x64 installer should still get final verification on CI (`windows-latest`) or real x64 hardware.
- The image is pinned to `dockurr/windows-arm:5.16` — v6.00 (2026-07-08) crash-loops with `Invalid MASK` because its `network.sh` calls `ipcalc`, which the image doesn't ship. Bump the `IMAGE` var in the script once fixed upstream.
- Installer URLs in the OEM scripts are version-pinned — bump them in `scripts/windows-vm.sh` when they 404.
