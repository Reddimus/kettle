//! Give Windows test harnesses an explicit non-elevated execution manifest.
//!
//! Windows installer detection treats an unmanifested executable whose name
//! contains `update` as a possible installer. The unit-test harness inherits
//! this package's `kettle_update-<hash>.exe` name, so a standard user gets
//! `ERROR_ELEVATION_REQUIRED` before a single test can run. The library and
//! shipped `kettle.exe` need no elevation; make that policy explicit for every
//! test target instead of requiring contributors to elevate Cargo.

#[cfg(target_os = "windows")]
fn main() {
    // rustc links test harnesses with `/MANIFEST:NO`; adding `/MANIFESTUAC`
    // through Cargo therefore leaves no resource to carry the policy, and
    // linker-argument order makes trying to override that setting brittle.
    // Compile the manifest as a real resource instead, just as the top-level
    // executable embeds its icon.
    let mut resource = winresource::WindowsResource::new();
    resource.set_manifest(
        r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#,
    );
    resource
        .compile()
        .expect("compile the Windows asInvoker test manifest");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
