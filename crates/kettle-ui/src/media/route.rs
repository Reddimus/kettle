//! Where a `show` goes: the pane its caller runs in, by process ancestry,
//! across every window. Full control may name a pane for a caller Kettle
//! cannot place, and so may the caller's own environment when it names this
//! Kettle; both routes stay unverified. Nothing else routes: not the focused
//! pane, and not another Kettle.

use kettle_ctl::process::ProcessIdentity;
use kettle_media::FailureCode;

use crate::ctl_server::CallerEvidence;

/// A live pane's own child process: a caller descended from it runs there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneRoot {
    pub root: ProcessIdentity,
    pub pane: u64,
    pub window: u64,
}

/// The pane a `show` lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    pub pane: u64,
    pub window: u64,
    /// The caller runs in this pane, by its ancestry.
    pub verified: bool,
}

/// The nearest of the caller's ancestors that is a live pane's child, as
/// `(pane, window)`.
pub(crate) fn nearest_pane(chain: &[ProcessIdentity], roots: &[PaneRoot]) -> Option<(u64, u64)> {
    chain.iter().find_map(|identity| {
        roots
            .iter()
            .find(|root| root.root == *identity)
            .map(|root| (root.pane, root.window))
    })
}

/// Route a `show`. `panes` lists every live pane as `(pane, window)`, and
/// `explicit` is the request's own `pane`, which only `full_control` may use.
pub(crate) fn route(
    caller: &CallerEvidence,
    explicit: Option<u64>,
    full_control: bool,
    roots: &[PaneRoot],
    panes: &[(u64, u64)],
    kettle_pid: u32,
) -> Result<Route, FailureCode> {
    if let Some(Ok(chain)) = caller.chain()
        && let Some((pane, window)) = nearest_pane(chain, roots)
    {
        return Ok(Route {
            pane,
            window,
            verified: true,
        });
    }
    let live = |pane: u64| {
        panes
            .iter()
            .find(|(id, _)| *id == pane)
            .map(|&(pane, window)| Route {
                pane,
                window,
                verified: false,
            })
    };
    if full_control && let Some(pane) = explicit {
        return live(pane).ok_or(FailureCode::NotInKettlePane);
    }
    // Pane ids restart with every Kettle, so a hint counts only when it
    // names this one.
    caller
        .hint()
        .filter(|hint| hint.kettle_pid == kettle_pid)
        .and_then(|hint| live(hint.pane))
        .ok_or(FailureCode::NotInKettlePane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctl_server::PaneHint;
    use kettle_ctl::identity::UnverifiedReason;

    fn id(pid: u32) -> ProcessIdentity {
        ProcessIdentity::new_for_tests(pid, u64::from(pid) * 10)
    }

    /// Pane 1 in window 7 runs shell 100; pane 2 in window 8 runs 200,
    /// which started 210 in it; pane 3 has no live child.
    fn roots() -> Vec<PaneRoot> {
        vec![
            PaneRoot {
                root: id(100),
                pane: 1,
                window: 7,
            },
            PaneRoot {
                root: id(200),
                pane: 2,
                window: 8,
            },
            PaneRoot {
                root: id(210),
                pane: 4,
                window: 8,
            },
        ]
    }

    const PANES: [(u64, u64); 4] = [(1, 7), (2, 8), (3, 8), (4, 8)];
    const KETTLE: u32 = 50;

    fn caller(
        chain: Result<Vec<ProcessIdentity>, UnverifiedReason>,
        hint: Option<(u64, u32)>,
    ) -> CallerEvidence {
        CallerEvidence::for_tests(
            chain,
            hint.map(|(pane, kettle_pid)| PaneHint { pane, kettle_pid }),
        )
    }

    fn route_for(
        caller: &CallerEvidence,
        explicit: Option<u64>,
        full: bool,
    ) -> Result<Route, FailureCode> {
        route(caller, explicit, full, &roots(), &PANES, KETTLE)
    }

    #[test]
    fn the_nearest_live_pane_ancestor_wins_in_any_window() {
        // 300 runs under 210, which runs under 200: the nearer pane is 4.
        let nested = caller(Ok(vec![id(300), id(210), id(200), id(KETTLE)]), None);
        assert_eq!(
            route_for(&nested, None, false),
            Ok(Route {
                pane: 4,
                window: 8,
                verified: true
            })
        );
        let shell = caller(Ok(vec![id(150), id(100), id(KETTLE)]), None);
        assert_eq!(
            route_for(&shell, None, false).map(|route| route.pane),
            Ok(1)
        );
    }

    #[test]
    fn a_reused_pid_is_not_an_ancestor() {
        let recycled = ProcessIdentity::new_for_tests(100, 999);
        let stranger = caller(Ok(vec![id(150), recycled, id(KETTLE)]), None);
        assert_eq!(
            route_for(&stranger, None, false),
            Err(FailureCode::NotInKettlePane)
        );
    }

    #[test]
    fn a_verified_caller_keeps_its_pane_whatever_it_asks_for() {
        let shell = caller(Ok(vec![id(150), id(100)]), Some((2, KETTLE)));
        for full in [false, true] {
            assert_eq!(
                route_for(&shell, Some(3), full).map(|route| route.pane),
                Ok(1)
            );
        }
    }

    #[test]
    fn only_full_control_names_a_pane_for_an_unplaced_caller() {
        let outsider = caller(Err(UnverifiedReason::NoPane), None);
        assert_eq!(
            route_for(&outsider, Some(3), true),
            Ok(Route {
                pane: 3,
                window: 8,
                verified: false
            })
        );
        assert_eq!(
            route_for(&outsider, Some(3), false),
            Err(FailureCode::NotInKettlePane)
        );
        assert_eq!(
            route_for(&outsider, Some(99), true),
            Err(FailureCode::NotInKettlePane)
        );
    }

    #[test]
    fn a_hint_routes_unverified_only_for_this_kettle_and_a_live_pane() {
        for chain in [
            Err(UnverifiedReason::MissingClaim),
            Ok(vec![id(150), id(1)]),
        ] {
            let hinted = caller(chain.clone(), Some((3, KETTLE)));
            assert_eq!(
                route_for(&hinted, None, false),
                Ok(Route {
                    pane: 3,
                    window: 8,
                    verified: false
                })
            );
            for hint in [(3, KETTLE + 1), (99, KETTLE)] {
                assert_eq!(
                    route_for(&caller(chain.clone(), Some(hint)), None, false),
                    Err(FailureCode::NotInKettlePane)
                );
            }
        }
    }

    #[test]
    fn nothing_else_routes() {
        let unchecked = CallerEvidence::default();
        assert_eq!(
            route_for(&unchecked, None, true),
            Err(FailureCode::NotInKettlePane)
        );
    }
}
