#[cfg(target_os = "windows")]
#[test]
fn production_windows_build_has_no_managed_update_target() {
    // Integration tests compile the library without cfg(test), unlike its unit
    // tests. This pins the retired production contract while the unit suite
    // continues to exercise the historical Windows installer implementation.
    assert_eq!(kettle_update::current_target(), None);
}
