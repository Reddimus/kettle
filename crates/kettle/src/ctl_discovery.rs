//! Where this binary looks for a running Kettle. Every lookup goes through
//! here.
//!
//! Unit tests run in the developer's own environment, often inside a Kettle.
//! There a real lookup reads the account's control registry, withdraws dead
//! pointers from it, and can reach the Kettle the developer is using and send
//! it requests. Under `cfg(test)` these find nothing, as on a CI runner with
//! no Kettle. A `kettle` a test starts is kept away from the account's
//! registry by its environment instead
//! (`kettle_test_support::isolate_ctl_registry`).

use kettle_ctl::{Client, CtlError};

/// [`Client::discover`].
pub(crate) fn discover(pid: Option<u32>) -> Result<Client, CtlError> {
    #[cfg(test)]
    {
        let _ = pid;
        Err(CtlError::NoServer)
    }
    #[cfg(not(test))]
    Client::discover(pid)
}

/// [`Client::discover_display`].
pub(crate) fn discover_display(pid: Option<u32>) -> Result<Client, CtlError> {
    #[cfg(test)]
    {
        let _ = pid;
        Err(CtlError::NotInKettle)
    }
    #[cfg(not(test))]
    Client::discover_display(pid)
}

/// [`kettle_ctl::client::display_target`].
pub(crate) fn display_target() -> Option<u32> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    kettle_ctl::client::display_target()
}

#[cfg(test)]
mod tests {
    /// Every lookup in this crate goes through this module, so none can
    /// reach the account's registry from a unit test: anywhere else, a call
    /// to one of these names is refused unless `ctl_discovery::` qualifies
    /// it.
    #[test]
    fn only_this_module_looks_for_a_running_kettle() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let names = [
            "discover",
            "discover_display",
            "display_target",
            "live_candidates",
            "registry_locations",
            "canonical_registry_dir",
            "prune_stale_aliases",
        ]
        .map(|name| format!("{name}("));
        let mut stack = vec![src.clone()];
        let mut scanned = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs")
                    || path == src.join("ctl_discovery.rs")
                {
                    continue;
                }
                scanned += 1;
                let code = kettle_test_support::code_only(&kettle_test_support::production_source(
                    &std::fs::read_to_string(&path).unwrap(),
                ));
                for name in &names {
                    for (at, _) in code.match_indices(name.as_str()) {
                        let before = &code[..at];
                        let longer_name = before
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_');
                        assert!(
                            longer_name
                                || before.ends_with("fn ")
                                || before.ends_with("ctl_discovery::"),
                            "{} calls {name}..) directly; go through crate::ctl_discovery",
                            path.display()
                        );
                    }
                }
            }
        }
        assert!(scanned > 10, "scanned only {scanned} files");
    }
}
