//! The wire specification in docs/MEDIA-PROTOCOL.md names the protocol version a
//! peer must send. It once said version 8 for a build that spoke 9, so a peer
//! written from it was answered RestartRequired on its first frame.

use std::path::Path;

#[test]
fn the_wire_specification_names_the_current_protocol_version() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/MEDIA-PROTOCOL.md");
    let doc = std::fs::read_to_string(&path).expect("read docs/MEDIA-PROTOCOL.md");
    let version = kettle_media::PROTOCOL_VERSION;
    assert!(
        doc.contains(&format!("## Wire version {version}\n")),
        "docs/MEDIA-PROTOCOL.md's wire heading must name protocol version {version}"
    );
    assert!(
        doc.contains(&format!("u16 LE protocol version {version},")),
        "docs/MEDIA-PROTOCOL.md's frame header must name protocol version {version}"
    );
}
