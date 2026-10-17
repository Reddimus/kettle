#[test]
fn placeholder_scan_is_allocation_bounded_before_resolution() {
    let src = super::production_source();
    let body = src
        .split("fn placeholder_cells_from_term(")
        .nth(1)
        .and_then(|rest| rest.split("fn virtuals_snapshot(").next())
        .expect("placeholder scan body");
    assert!(
        body.contains("CellDiacritics::parse(cell.zerowidth().unwrap_or(&[]))"),
        "combining-mark slices must not be cloned per placeholder cell"
    );
    assert!(
        body.contains("placeholder::is_placeholder(cell.c)"),
        "placeholder scans must use the shared protocol predicate"
    );
    assert!(
        body.contains("if placeholder_count >= placement_limit"),
        "the scan must stop materializing cells at the placement cap"
    );
}

#[test]
fn kitty_delete_skips_origin_snapshots_without_relative_placements() {
    let src = super::production_source();
    assert!(
        src.matches("if relative_snapshot.is_empty() {").count() >= 2,
        "both kitty-delete paths must skip the grid/image origin snapshots when no relative placement can consume them"
    );
}

#[test]
fn placeholder_tiles_reaches_virtuals_only_through_the_owned_snapshot() {
    // Normalized, like every other source guard in this file: the split
    // patterns below embed `\n`, so a CRLF checkout would silently find
    // nothing and fail on an unrelated-looking `expect`.
    let src = super::production_source();
    let body = src
        .split("pub fn placeholder_tiles(&self) -> Vec<Placement> {")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub fn ").next())
        .expect("placeholder_tiles body");

    assert!(
        !body.contains("virtuals.lock()"),
        "placeholder_tiles must not lock `virtuals` itself; taking the \
             guard here lets a later edit hold it across `placeholder_cells`, \
             which is the ABBA deadlock the reader's `term` -> `virtuals` \
             order creates"
    );

    // `virtuals_snapshot` must be the thing that locks — otherwise the
    // assertion above passes on a `placeholder_tiles` that gets its guard
    // from somewhere else and the owned signature guards nothing.
    //
    // Deliberately not a count over the module: the PTY reader locks
    // `virtuals` in six other places and is right to, because it already
    // holds `term` and so takes them in the safe order. A "exactly one lock
    // site" assertion reads well and is simply false about this file.
    let snapshot_body = src
        .split("fn virtuals_snapshot(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .and_then(|rest| rest.split("\n    pub fn ").next())
        .expect("virtuals_snapshot body");
    assert!(
        snapshot_body.contains("virtuals.lock()"),
        "virtuals_snapshot must be the lock site `placeholder_tiles` goes \
             through; if it stops locking, the owned signature below is \
             guarding nothing"
    );

    let snapshot_at = body
        .find("self.virtuals_snapshot()")
        .expect("placeholder_tiles must consult the virtual map");
    let cells_at = body
        .find("self.placeholder_cells()")
        .expect("placeholder_tiles must read the grid");
    assert!(
        snapshot_at < cells_at,
        "placeholder_tiles must consult `virtuals` before walking the grid; \
             otherwise every pane pays a full visible-cell scan per frame to \
             discover it has no virtual placements"
    );

    // The owned signature is what makes the release a compile error rather
    // than a review note. Pin it: widening the return type to borrow from
    // the guard would silently restore the deadlock.
    // All whitespace removed, and the trailing comma rustfmt only emits in
    // the wrapped form dropped, so the signature may be rewrapped freely.
    let signature = src
        .split("fn virtuals_snapshot(")
        .nth(1)
        .and_then(|rest| rest.split(" {\n").next())
        .expect("virtuals_snapshot signature")
        .split_whitespace()
        .collect::<String>()
        .replace(",)", ")");
    assert_eq!(
        signature, "&self)->Option<(HashMap<(u32,u32),VirtualEntry>,HashMap<u32,u32>)>",
        "virtuals_snapshot must keep returning owned maps; a borrowed \
             return type would let the `virtuals` guard escape to a caller \
             that then locks `term`"
    );
}

/// `relative_tiles` is the in-repo precedent: snapshot under one lock, drop
/// it, then take the others. Keep its comment honest so the discipline is
/// discoverable from either function.
#[test]
fn relative_tiles_still_documents_the_single_acquisition_order() {
    // Normalized, like every other source guard in this file: the split
    // patterns below embed `\n`, so a CRLF checkout would silently find
    // nothing and fail on an unrelated-looking `expect`.
    let src = super::production_source();
    let body = src
        .split("pub fn relative_tiles(&self) -> Vec<Placement> {")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub fn ").next())
        .expect("relative_tiles body");
    assert!(
        body.contains("single lock-acquisition order"),
        "relative_tiles must keep stating why it snapshots before locking"
    );
}
