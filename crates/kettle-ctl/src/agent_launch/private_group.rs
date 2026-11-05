/// The owner and group facts the private-group rule reads.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct GroupFacts<'a> {
    pub(super) owner_name: &'a [u8],
    pub(super) owner_primary_gid: u32,
    pub(super) gid: u32,
    pub(super) group_name: &'a [u8],
    pub(super) group_has_members: bool,
}

/// A user private group: the owner's primary group, named after the owner,
/// listing no supplementary members or other non-root primary members.
pub(super) fn is_private_group(
    facts: &GroupFacts,
    owner: u32,
    accounts: impl Iterator<Item = std::io::Result<(u32, u32)>>,
) -> bool {
    if facts.owner_primary_gid != facts.gid
        || facts.group_name != facts.owner_name
        || facts.group_has_members
    {
        return false;
    }
    let mut owner_seen = false;
    for (index, account) in accounts.enumerate() {
        if index == 65536 {
            return false;
        }
        let Ok((uid, gid)) = account else {
            return false;
        };
        if gid == facts.gid {
            if uid != owner && uid != 0 {
                return false;
            }
            owner_seen |= uid == owner;
        }
    }
    owner_seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private() -> GroupFacts<'static> {
        GroupFacts {
            owner_name: b"alice",
            owner_primary_gid: 1000,
            gid: 1000,
            group_name: b"alice",
            group_has_members: false,
        }
    }

    #[test]
    fn another_accounts_primary_group_is_not_private() {
        let accounts = [Ok((1000, 1000)), Ok((1001, 1000))];
        assert!(
            !is_private_group(&private(), 1000, accounts.into_iter()),
            "another non-root account shares the owner's primary GID"
        );
    }

    #[test]
    fn primary_group_requires_complete_bounded_account_evidence() {
        assert!(is_private_group(
            &private(),
            1000,
            [Ok((0, 1000)), Ok((1000, 1000)), Ok((1001, 1001))].into_iter()
        ));
        assert!(!is_private_group(&private(), 1000, std::iter::empty()));
        assert!(!is_private_group(
            &private(),
            1000,
            [Ok((1000, 1000)), Err(std::io::Error::other("NSS failure"))].into_iter()
        ));
        assert!(is_private_group(
            &private(),
            1000,
            std::iter::repeat_with(|| Ok((1000, 1000))).take(65536)
        ));
        assert!(!is_private_group(
            &private(),
            1000,
            std::iter::repeat_with(|| Ok((1000, 1000)))
        ));
    }
}
