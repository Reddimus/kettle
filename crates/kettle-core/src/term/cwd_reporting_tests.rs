use super::reported_current_dir;

#[test]
fn launch_seed_is_not_reported_until_an_osc_cwd_arrives() {
    assert_eq!(
        reported_current_dir(false, Some("C:\\launch-seed".to_owned())),
        None
    );
    assert_eq!(
        reported_current_dir(true, Some("/shell/reported".to_owned())),
        Some("/shell/reported".to_owned())
    );
}
