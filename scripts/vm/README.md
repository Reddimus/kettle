# Ubuntu ARM test guest

`run-ubuntu-arm.sh` starts the maintained Ubuntu 26.04 aarch64 test guest
directly under QEMU with Apple's HVF accelerator. The guest was migrated from
the retired Parallels VM without replacing its OS, user, toolchain, desktop, or
repository state.

The default launch is headless and forwards SSH to `127.0.0.1:2222`:

```sh
scripts/vm/run-ubuntu-arm.sh
ssh kettle-vm
```

Use the Cocoa virtio framebuffer for live-window checks:

```sh
scripts/vm/run-ubuntu-arm.sh gui
```

The private qcow2 and mutable EFI variables live outside the checkout under
`~/VMs`. They contain the migrated home directory and credentials and must
never be committed. The launcher restricts their modes and uses SSH as the
automation boundary so commands run as the guest user rather than root.

Shut down cleanly before inspecting the disk:

```sh
ssh kettle-vm 'sudo systemctl poweroff'
qemu-img check ~/VMs/ubuntu-arm.qcow2
```

Run `sudo fstrim -av` in the guest before shutdown when substantial build data
has been removed. The launcher enables discard so freed blocks return to the
sparse qcow2.

The migration expanded the root disk from 128 GiB to 256 GiB. On 2026-08-23,
the guest completed 2,131 workspace tests across 45 binaries and linked the
release binary. The `search-history` live-window smoke passed under Xvfb and in
a real GNOME Wayland session. Both runs selected Vulkan through Mesa llvmpipe
and reported a CPU adapter; this proves software rendering, not accelerated
virtio GPU rendering.
