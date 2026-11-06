//! The swapchain present mode a window surface asks for.
use crate::gpu::desired_present_mode;
use wgpu::PresentMode;

#[test]
fn a_wayland_surface_presents_in_mailbox_when_it_is_offered() {
    // Mesa's FIFO on Wayland can hold a commit with no frame callback after a
    // fullscreen change on GNOME Shell 50; the window then stops repainting.
    let modes = [PresentMode::Fifo, PresentMode::Mailbox];
    assert_eq!(desired_present_mode(&modes, true), PresentMode::Mailbox);
}

#[test]
fn a_wayland_surface_without_mailbox_keeps_vsync() {
    let modes = [PresentMode::Fifo];
    assert_eq!(desired_present_mode(&modes, true), PresentMode::AutoVsync);
}

#[test]
fn other_window_systems_keep_vsync() {
    let modes = [
        PresentMode::Fifo,
        PresentMode::Mailbox,
        PresentMode::Immediate,
    ];
    assert_eq!(desired_present_mode(&modes, false), PresentMode::AutoVsync);
}
