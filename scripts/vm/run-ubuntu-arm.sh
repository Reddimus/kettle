#!/usr/bin/env bash
# Launch the Linux ARM test guest under QEMU with HVF.
#
# This replaces the Parallels `Ubuntu 26.04` VM retired on 2026-08-23. The disk
# is that guest's own filesystem converted to qcow2, not a fresh install, so it
# keeps the toolchain, desktop session, and configuration the old one had.
#
#   run-ubuntu-arm.sh            headless; SSH on localhost:2222
#   run-ubuntu-arm.sh gui        virtio framebuffer + Cocoa window for live UI
#
# Drive it over SSH rather than a guest-agent exec channel. The old setup ran
# every command as root against a uid-1000-owned tree and left 67,466 root-owned
# files under ~/.rustup and ~/.cargo; SSH runs as the real user and cannot.
set -euo pipefail
umask 077

VM_DIR="${KETTLE_VM_DIR:-$HOME/VMs}"
DISK="$VM_DIR/ubuntu-arm.qcow2"
EFI_VARS="$VM_DIR/efi_vars.fd"
QGA_SOCKET="$VM_DIR/qemu-guest-agent.sock"
EFI_CODE="${KETTLE_VM_EFI_CODE:-/opt/homebrew/share/qemu/edk2-aarch64-code.fd}"
SSH_PORT="${KETTLE_VM_SSH_PORT:-2222}"
CPUS="${KETTLE_VM_CPUS:-8}"
MEM_MB="${KETTLE_VM_MEM_MB:-16384}"

for f in "$DISK" "$EFI_CODE"; do
  [ -r "$f" ] || { echo "missing: $f" >&2; exit 1; }
done
# The disk contains the migrated user's home directory and credentials. Keep
# the default storage directory and mutable VM files private on every launch.
chmod 700 "$VM_DIR"
chmod 600 "$DISK"
# The firmware needs writable variable storage; seed it once from the template.
[ -f "$EFI_VARS" ] || cp "${KETTLE_VM_EFI_VARS_TEMPLATE:-/opt/homebrew/share/qemu/edk2-arm-vars.fd}" "$EFI_VARS"
chmod 600 "$EFI_VARS"

case "${1:-none}" in
  gui)  display=(-device virtio-gpu-pci -display cocoa) ;;
  none) display=(-display none) ;;
  *) echo "usage: $0 [none|gui]" >&2; exit 2 ;;
esac

# The guest agent is not the automation boundary, SSH is. Giving the installed
# qemu-guest-agent its standard virtio port still provides clean host-visible
# health and shutdown support. Refuse to replace anything except an inactive
# socket left behind by an unclean QEMU exit; unlinking a live listener would
# sever guest-agent access to the already-running VM.
if [ -e "$QGA_SOCKET" ] || [ -L "$QGA_SOCKET" ]; then
  [ -S "$QGA_SOCKET" ] && [ ! -L "$QGA_SOCKET" ] \
    || { echo "refusing non-socket guest-agent path: $QGA_SOCKET" >&2; exit 1; }
  if lsof "$QGA_SOCKET" >/dev/null 2>&1; then
    echo "guest-agent socket is active; is the VM already running?" >&2
    exit 1
  fi
  rm "$QGA_SOCKET"
fi

# discard=unmap keeps the image honest: without it `fstrim` in the guest cannot
# punch freed blocks back out and the qcow2 only ever grows.
exec qemu-system-aarch64 \
  -machine virt,accel=hvf,highmem=on \
  -cpu host -smp "$CPUS" -m "$MEM_MB" \
  -drive "if=pflash,format=raw,readonly=on,file=$EFI_CODE" \
  -drive "if=pflash,format=raw,file=$EFI_VARS" \
  -drive "if=none,id=hd0,file=$DISK,format=qcow2,discard=unmap,cache=writeback" \
  -device virtio-blk-pci,drive=hd0 \
  -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:22" \
  -device virtio-net-pci,netdev=net0 \
  -device virtio-serial-pci \
  -chardev "socket,path=$QGA_SOCKET,server=on,wait=off,id=qga0" \
  -device virtserialport,chardev=qga0,name=org.qemu.guest_agent.0 \
  -device qemu-xhci -device usb-kbd -device usb-tablet \
  -device virtio-rng-pci \
  "${display[@]}"
