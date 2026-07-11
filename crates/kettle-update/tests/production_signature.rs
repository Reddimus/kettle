#[test]
fn production_key_accepts_openssl_release_signature_fixture() {
    let manifest = include_bytes!("fixtures/manifest-v1.json");
    let signature = include_bytes!("fixtures/manifest-v1.json.sig");
    let parsed =
        kettle_update::verify_manifest(manifest, signature, &kettle_update::UPDATE_PUBLIC_KEY)
            .expect("fixture generated with the offline production release key must verify");
    assert_eq!(parsed.schema, 1);
    assert_eq!(parsed.tag, "v999.0.0");
    assert_eq!(parsed.assets.len(), 3);
}
