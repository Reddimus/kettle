use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use super::{LaunchBinding, LaunchFiles, PickupBook, PickupPermit};
use crate::process::{self, ProcessIdentity};
use crate::show::InlineTarget;

const MAX_LAUNCHES: usize = 64;

fn live(identity: ProcessIdentity) -> bool {
    process::inspect(identity.pid()).is_ok_and(|now| now.identity == identity && !now.exited)
}

/// Context established from checked kernel ancestry and the GUI's live panes.
#[derive(Clone, Copy, Debug)]
pub struct CardCaller<'a> {
    pub target: InlineTarget,
    pub owner: ProcessIdentity,
    pub chain: &'a [ProcessIdentity],
    pub pane: u64,
    pub pane_root: ProcessIdentity,
}

#[derive(Debug)]
struct Scope {
    binding: LaunchBinding,
    files: LaunchFiles,
}

/// A launch witness retained through asynchronous rendering.
#[derive(Clone, Debug)]
pub struct LaunchGrant {
    scope: Arc<Scope>,
    owner: ProcessIdentity,
}

impl LaunchGrant {
    pub fn owner(&self) -> ProcessIdentity {
        self.owner
    }
    pub fn pane(&self) -> u64 {
        self.scope.binding.pane()
    }

    pub fn valid(&self, pane: u64, pane_root: ProcessIdentity) -> bool {
        let binding = self.scope.binding;
        pane == binding.pane()
            && pane_root == binding.pane_root()
            && live(pane_root)
            && self.scope.files.matches(binding.root(), self.owner)
            && if self.scope.files.launcher.is_some() {
                process::inspect(self.owner.pid())
                    .is_ok_and(|owner| owner.parent_pid == Some(binding.root().pid()))
            } else {
                self.owner == binding.root()
            }
    }
}

/// GUI-owned launch scopes and hook permits. No capability is published in
/// terminal state or passed to the agent's environment.
#[derive(Default)]
pub struct LaunchScopes {
    scopes: HashMap<ProcessIdentity, Arc<Scope>>,
    pickups: PickupBook<LaunchGrant>,
}

impl LaunchScopes {
    pub fn prune(&mut self, now: Instant) {
        self.scopes.retain(|root, scope| {
            live(*root)
                && live(scope.binding.pane_root())
                && scope.files.native.unchanged()
                && scope
                    .files
                    .launcher
                    .as_ref()
                    .is_none_or(|file| file.unchanged())
        });
        self.pickups.prune(now);
    }

    /// Registration occurs before exec, so only liveness and held-file
    /// identity are checked here. Granting a card also checks the running image.
    pub fn register(
        &mut self,
        root: ProcessIdentity,
        pane: u64,
        pane_root: ProcessIdentity,
        files: LaunchFiles,
        now: Instant,
    ) -> bool {
        self.prune(now);
        if self.scopes.len() >= MAX_LAUNCHES
            || self.scopes.contains_key(&root)
            || !live(root)
            || !live(pane_root)
            || !files.native.unchanged()
            || files
                .launcher
                .as_ref()
                .is_some_and(|file| !file.unchanged())
        {
            return false;
        }
        let binding = LaunchBinding::new(root, pane, pane_root, files.launcher.is_some());
        self.scopes.insert(root, Arc::new(Scope { binding, files }));
        true
    }

    pub fn grant(&mut self, caller: CardCaller<'_>, now: Instant) -> Option<LaunchGrant> {
        self.prune(now);
        for root in caller.chain.iter().skip(1).take(2) {
            let Some(scope) = self.scopes.get(root) else {
                continue;
            };
            if scope.files.target != caller.target
                || !scope
                    .binding
                    .matches(caller.pane, caller.pane_root, caller.owner, caller.chain)
            {
                continue;
            }
            let grant = LaunchGrant {
                scope: scope.clone(),
                owner: caller.owner,
            };
            if grant.valid(caller.pane, caller.pane_root) {
                return Some(grant);
            }
        }
        None
    }

    pub fn issue(
        &mut self,
        grant: LaunchGrant,
        pane_root: ProcessIdentity,
        now: Instant,
        mut random: impl FnMut(&mut [u8]) -> bool,
    ) -> Option<PickupPermit> {
        if !grant.valid(grant.pane(), pane_root) {
            return None;
        }
        for _ in 0..4 {
            let mut bytes = [0; 16];
            if !random(&mut bytes) {
                return None;
            }
            let key = PickupPermit::new(bytes);
            if self.pickups.issue(key, grant.clone(), now) {
                return Some(key);
            }
        }
        None
    }

    pub fn redeem(&mut self, key: PickupPermit, caller: CardCaller<'_>, now: Instant) -> bool {
        let Some(grant) = self.pickups.take(key, now) else {
            return false;
        };
        grant.owner == caller.owner
            && grant.scope.files.target == caller.target
            && grant.scope.binding.matches(
                caller.pane,
                caller.pane_root,
                caller.owner,
                caller.chain,
            )
            && grant.valid(caller.pane, caller.pane_root)
    }

    pub fn owner_live(&self, owner: ProcessIdentity) -> bool {
        let valid = |scope: &Arc<Scope>| {
            LaunchGrant {
                scope: scope.clone(),
                owner,
            }
            .valid(scope.binding.pane(), scope.binding.pane_root())
        };
        if self.scopes.get(&owner).is_some_and(valid) {
            return true;
        }
        let Some(parent) = process::inspect(owner.pid())
            .ok()
            .filter(|now| now.identity == owner && !now.exited)
            .and_then(|now| now.parent_pid)
            .and_then(|pid| process::identity(pid).ok())
        else {
            return false;
        };
        self.scopes
            .get(&parent)
            .is_some_and(|scope| scope.files.launcher.is_some() && valid(scope))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_launch::HeldExecutable;

    fn me() -> (ProcessIdentity, Arc<HeldExecutable>) {
        (
            process::current().unwrap().identity,
            Arc::new(HeldExecutable::open(&std::env::current_exe().unwrap()).unwrap()),
        )
    }

    #[test]
    fn grants_and_one_use_pickups_keep_the_original_pane_generation() {
        let (root, native) = me();
        let now = Instant::now();
        let mut scopes = LaunchScopes::default();
        assert!(scopes.register(
            root,
            7,
            root,
            LaunchFiles {
                target: InlineTarget::CodexHook,
                native,
                launcher: None
            },
            now
        ));
        let chain = [ProcessIdentity::new_for_tests(400, 4), root];
        let caller = CardCaller {
            target: InlineTarget::CodexHook,
            owner: root,
            chain: &chain,
            pane: 7,
            pane_root: root,
        };
        let grant = scopes.grant(caller, now).expect("launched harness");
        assert!(
            scopes
                .grant(CardCaller { pane: 8, ..caller }, now)
                .is_none()
        );
        let changed_root = ProcessIdentity::new_for_tests(root.pid(), root.start() + 1);
        assert!(
            scopes
                .grant(
                    CardCaller {
                        pane_root: changed_root,
                        ..caller
                    },
                    now
                )
                .is_none()
        );
        let key = scopes
            .issue(grant.clone(), root, now, |bytes| {
                bytes.fill(1);
                true
            })
            .unwrap();
        assert!(scopes.redeem(key, caller, now));
        assert!(!scopes.redeem(key, caller, now));
        let key = scopes
            .issue(grant, root, now, |bytes| {
                bytes.fill(2);
                true
            })
            .unwrap();
        assert!(!scopes.redeem(
            key,
            CardCaller {
                pane_root: changed_root,
                ..caller
            },
            now
        ));
        assert!(
            !scopes.redeem(key, caller, now),
            "a rejected pickup is consumed"
        );
    }

    struct Child(Option<std::process::Child>);
    impl Child {
        fn stop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                child.wait().unwrap();
            }
        }
    }
    impl Drop for Child {
        fn drop(&mut self) {
            self.stop();
        }
    }

    #[test]
    fn a_native_child_is_checked_again_when_its_hook_collects_the_card() {
        let (root, launcher) = me();
        let native = Arc::new(HeldExecutable::open(std::path::Path::new("/bin/sleep")).unwrap());
        let child = std::process::Command::new(native.path())
            .arg("30")
            .spawn()
            .unwrap();
        println!("owned native identity fixture child pid={}", child.id());
        let owner = process::identity(child.id()).unwrap();
        let mut child = Child(Some(child));
        let now = Instant::now();
        let mut scopes = LaunchScopes::default();
        assert!(scopes.register(
            root,
            9,
            root,
            LaunchFiles {
                target: InlineTarget::CodexHook,
                native,
                launcher: Some(launcher)
            },
            now
        ));
        let chain = [ProcessIdentity::new_for_tests(400, 4), owner, root];
        let caller = CardCaller {
            target: InlineTarget::CodexHook,
            owner,
            chain: &chain,
            pane: 9,
            pane_root: root,
        };
        let grant = scopes.grant(caller, now).expect("native child grant");
        assert!(scopes.owner_live(owner));
        let key = scopes
            .issue(grant, root, now, |bytes| {
                bytes.fill(3);
                true
            })
            .unwrap();
        child.stop();
        assert!(
            !scopes.redeem(key, caller, now),
            "exited child kept its permit"
        );
        assert!(!scopes.owner_live(owner));
    }
}
