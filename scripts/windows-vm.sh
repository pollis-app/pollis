#!/usr/bin/env bash
# Windows 11 ARM64 dev VM in Docker — self-contained recipe.
#
# Stack: colima (vz + nested virtualization, Apple Silicon M3+/macOS 15+)
#        -> Linux VM with KVM -> dockur/windows-arm container -> Windows 11.
# Docker Desktop cannot run this (no KVM on macOS); colima is required.
# All state lives outside the repo in ~/.pollis/windows-vm + a docker volume.
# Docs: .codesight/wiki/windows-vm.md
set -euo pipefail

# v6.00 (2026-07-08) crash-loops: its network.sh needs ipcalc, which the
# image doesn't ship ("Invalid MASK" on boot). Bump when fixed upstream.
IMAGE="dockurr/windows-arm:5.16"
PROFILE="windows"
PROJECT="pollis-windows"
STATE_DIR="${HOME}/.pollis/windows-vm"
COMPOSE="docker compose -p ${PROJECT} -f ${STATE_DIR}/compose.yaml"

write_config() {
  mkdir -p "${STATE_DIR}/oem" "${STATE_DIR}/shared"

  cat > "${STATE_DIR}/compose.yaml" <<EOF
services:
  windows:
    image: ${IMAGE}
    container_name: pollis-windows-dev
    environment:
      VERSION: "11"
      RAM_SIZE: "8G"
      CPU_CORES: "6"
      DISK_SIZE: "96G"
      USERNAME: "dev"
      PASSWORD: "pollis"
    devices:
      - /dev/kvm
      - /dev/net/tun
    cap_add:
      - NET_ADMIN
    ports:
      - 8006:8006
      - 3389:3389/tcp
      - 3389:3389/udp
    volumes:
      - windows-storage:/storage
      - ./shared:/shared
      - ./oem:/oem
    restart: on-failure:3
    stop_grace_period: 2m

volumes:
  windows-storage:
EOF

  # Runs elevated inside Windows once, at the end of unattended setup.
  # Installer URLs are version-pinned; bump when they 404.
  cat > "${STATE_DIR}/oem/install.bat" <<'EOF'
@echo off
set LOG=C:\OEM\install.log
echo ==== pollis dev setup start ==== >> %LOG% 2>&1

rem VS 2022 Build Tools: MSVC ARM64 + x64 cross tools + Win11 SDK
curl -L -o %TEMP%\vs_buildtools.exe https://aka.ms/vs/17/release/vs_buildtools.exe >> %LOG% 2>&1
%TEMP%\vs_buildtools.exe --quiet --wait --norestart --nocache ^
  --add Microsoft.VisualStudio.Workload.VCTools ^
  --add Microsoft.VisualStudio.Component.VC.Tools.ARM64 ^
  --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 ^
  --add Microsoft.VisualStudio.Component.Windows11SDK.26100 >> %LOG% 2>&1

rem Git for Windows (arm64)
curl -L -o %TEMP%\git-setup.exe https://github.com/git-for-windows/git/releases/download/v2.50.1.windows.1/Git-2.50.1-arm64.exe >> %LOG% 2>&1
%TEMP%\git-setup.exe /VERYSILENT /NORESTART >> %LOG% 2>&1

rem Node.js 22 LTS (arm64, machine-wide)
curl -L -o %TEMP%\node.msi https://nodejs.org/dist/v22.17.0/node-v22.17.0-arm64.msi >> %LOG% 2>&1
msiexec /i %TEMP%\node.msi /qn /norestart >> %LOG% 2>&1

rem pnpm node_modules and cargo target paths routinely exceed 260 chars
reg add "HKLM\SYSTEM\CurrentControlSet\Control\FileSystem" /v LongPathsEnabled /t REG_DWORD /d 1 /f >> %LOG% 2>&1

rem rustup and pnpm are per-user installs — defer to first logon
reg add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce" /v PollisDevSetup /t REG_SZ /d "cmd /c C:\OEM\setup-user.bat" /f >> %LOG% 2>&1

echo ==== pollis dev setup done ==== >> %LOG% 2>&1
EOF

  cat > "${STATE_DIR}/oem/setup-user.bat" <<'EOF'
@echo off
set LOG=C:\OEM\setup-user.log
echo ==== pollis user setup start ==== >> %LOG% 2>&1

rem Rust (native aarch64-pc-windows-msvc host toolchain)
curl -L -o %TEMP%\rustup-init.exe https://static.rust-lang.org/rustup/dist/aarch64-pc-windows-msvc/rustup-init.exe >> %LOG% 2>&1
%TEMP%\rustup-init.exe -y --default-host aarch64-pc-windows-msvc >> %LOG% 2>&1

rem pnpm (user-level, no elevation needed)
call npm install -g pnpm >> %LOG% 2>&1

echo ==== pollis user setup done ==== >> %LOG% 2>&1
EOF
}

ensure_colima() {
  if ! command -v colima >/dev/null; then
    echo "==> installing colima"
    HOMEBREW_NO_REQUIRE_TAP_TRUST=1 HOMEBREW_NO_AUTO_UPDATE=1 brew install colima
  fi
  if ! colima status -p "${PROFILE}" >/dev/null 2>&1; then
    echo "==> starting colima profile '${PROFILE}' (nested virtualization)"
    colima start "${PROFILE}" --vm-type vz --nested-virtualization \
      --cpu 8 --memory 12 --disk 120
  fi
  docker context use "colima-${PROFILE}" >/dev/null
}

case "${1:-}" in
  up)
    ensure_colima
    write_config
    ${COMPOSE} up -d
    echo
    echo "Viewer:  http://localhost:8006  (first boot: unattended install, ~15-30 min)"
    echo "RDP:     localhost:3389  user 'dev' / password 'pollis'"
    echo "Shared:  ${STATE_DIR}/shared  ->  \\\\host.lan\\Data in the guest"
    ;;
  stop)
    ${COMPOSE} stop
    ;;
  start)
    docker context use "colima-${PROFILE}" >/dev/null
    ${COMPOSE} start
    ;;
  down)
    if [[ "${2:-}" == "--wipe" ]]; then
      ${COMPOSE} down -v
      echo "Windows disk wiped."
    else
      ${COMPOSE} down
      echo "Container removed; Windows disk kept (re-run 'up' to boot it, 'down --wipe' to destroy)."
    fi
    ;;
  logs)
    docker logs -f pollis-windows-dev
    ;;
  status)
    colima status -p "${PROFILE}" 2>&1 || true
    docker ps -a --filter name=pollis-windows-dev \
      --format 'container: {{.Status}}'
    ;;
  suspend)
    ${COMPOSE} stop
    colima stop "${PROFILE}"
    echo "VM stopped and colima memory released."
    ;;
  *)
    echo "usage: $(basename "$0") up|stop|start|down [--wipe]|logs|status|suspend"
    exit 1
    ;;
esac
