/// The owner and group facts the private-group rule reads.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct GroupFacts<'a> {
    pub(super) owner_name: &'a [u8],
    pub(super) owner_primary_gid: u32,
    pub(super) gid: u32,
    pub(super) group_name: &'a [u8],
    pub(super) group_has_members: bool,
    pub(super) enumeration_is_exhaustive: bool,
}

/// A user private group: the owner's primary group, named after the owner,
/// listing no supplementary members or other non-root primary members.
pub(super) fn is_private_group(
    facts: &GroupFacts,
    owner: u32,
    accounts: impl Iterator<Item = std::io::Result<(u32, u32)>>,
) -> bool {
    if !facts.enumeration_is_exhaustive
        || facts.owner_primary_gid != facts.gid
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

pub(super) const MAX_NSSWITCH_BYTES: usize = 64 * 1024;

/// A clean getpwent EOF proves nothing about providers that omit accounts
/// from enumeration. Only the local enumerating sources can support it.
pub(super) fn passwd_enumeration_is_exhaustive(config: Result<&str, std::io::ErrorKind>) -> bool {
    let config = match config {
        Ok(config) => config,
        // A missing file uses glibc's default local files database.
        Err(error) => return error == std::io::ErrorKind::NotFound,
    };
    if config.len() > MAX_NSSWITCH_BYTES || config.contains('\0') {
        return false;
    }
    let mut passwd_seen = false;
    let mut logical = String::new();
    for physical in config.lines() {
        let line = physical.split('#').next().unwrap_or_default();
        if !line
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte.is_ascii_whitespace())
        {
            return false;
        }
        let line = line.trim();
        if let Some(line) = line.strip_suffix('\\') {
            logical.push_str(line);
            logical.push(' ');
            continue;
        }
        logical.push_str(line);
        let line = logical.trim();
        if !line.is_empty() {
            let Some((database, sources)) = line.split_once(':') else {
                return false;
            };
            let database = database.trim();
            if !nss_name(database) {
                return false;
            }
            let Some(local) = nss_sources_are_local(sources) else {
                return false;
            };
            if database == "passwd" {
                if passwd_seen || !local {
                    return false;
                }
                passwd_seen = true;
            }
        }
        logical.clear();
    }
    passwd_seen && logical.is_empty()
}

fn nss_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn nss_sources_are_local(mut sources: &str) -> Option<bool> {
    let mut source_seen = false;
    let mut all_local = true;
    loop {
        sources = sources.trim_start();
        if sources.is_empty() {
            return source_seen.then_some(all_local);
        }
        if let Some(actions) = sources.strip_prefix('[') {
            if !source_seen {
                return None;
            }
            let (actions, rest) = actions.split_once(']')?;
            if actions.trim().is_empty() {
                return None;
            }
            for action in actions.split_ascii_whitespace() {
                let (status, result) = action.split_once('=')?;
                if !matches!(
                    status.strip_prefix('!').unwrap_or(status),
                    "SUCCESS" | "NOTFOUND" | "UNAVAIL" | "TRYAGAIN"
                ) || !matches!(result, "return" | "continue" | "merge")
                {
                    return None;
                }
            }
            sources = rest;
        } else {
            let end = sources
                .find(|character: char| character.is_ascii_whitespace() || character == '[')
                .unwrap_or(sources.len());
            let source = &sources[..end];
            if !nss_name(source) {
                return None;
            }
            all_local &= matches!(
                source,
                "files" | "compat" | "systemd" | "altfiles" | "extrausers"
            );
            source_seen = true;
            sources = &sources[end..];
        }
    }
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
            enumeration_is_exhaustive: true,
        }
    }

    #[test]
    fn an_omitted_nss_peer_does_not_make_a_primary_group_private() {
        // SSSD can resolve a peer sharing GID 1000 while enumerating only
        // the local owner, even when getpwent reaches a clean EOF.
        for (config, expected) in [
            ("passwd: files", true),
            ("passwd: files systemd", true),
            ("passwd: files sss", false),
            ("passwd: files [NOTFOUND=return] ldap", false),
            ("passwd: files winbind", false),
            ("passwd: files nis", false),
            ("passwd: files unknown", false),
            ("passwd: files\npasswd: files", false),
        ] {
            let facts = GroupFacts {
                enumeration_is_exhaustive: passwd_enumeration_is_exhaustive(Ok(config)),
                ..private()
            };
            assert_eq!(
                is_private_group(&facts, 1000, [Ok((1000, 1000))].into_iter()),
                expected,
                "{config:?}"
            );
            assert!(!is_private_group(
                &facts,
                1000,
                [Ok((1000, 1000)), Ok((1001, 1000))].into_iter()
            ));
        }
        for (config, expected) in [
            (Err(std::io::ErrorKind::NotFound), true),
            (Err(std::io::ErrorKind::PermissionDenied), false),
            (Err(std::io::ErrorKind::InvalidData), false),
        ] {
            let facts = GroupFacts {
                enumeration_is_exhaustive: passwd_enumeration_is_exhaustive(config),
                ..private()
            };
            assert_eq!(
                is_private_group(&facts, 1000, [Ok((1000, 1000))].into_iter()),
                expected
            );
        }
    }

    #[test]
    fn nsswitch_parser_accepts_only_exhaustive_local_passwd_sources() {
        for config in [
            "passwd: files",
            "  passwd :\tfiles\t systemd  # local accounts\n",
            "# comment\ngroup: files sss\npasswd: files compat systemd altfiles extrausers",
            "passwd: files # non-ASCII comment: \u{00e9}",
            "passwd: files [NOTFOUND=return] systemd",
            "passwd: files[!UNAVAIL=return SUCCESS=continue] systemd",
            "passwd: files \\\n    systemd\n",
            "passwd: files \\\r\n\t[NOTFOUND=return] \\\r\n extrausers # comment",
        ] {
            assert!(passwd_enumeration_is_exhaustive(Ok(config)), "{config:?}");
        }
        for config in [
            "",
            "group: files",
            "passwd: files\npasswd: systemd",
            "passwd: files ldap",
            "passwd: sss files",
            "passwd: files [NOTFOUND=return] unknown",
            "passwd: files [NOTFOUND=return] nis",
            "passwd: winbind",
            "passwd:",
            "passwd files",
            "passwd: [NOTFOUND=return] files",
            "passwd: files [NOTFOUND=return",
            "passwd: files []",
            "passwd: files [UNKNOWN=return]",
            "passwd: files [NOTFOUND=unknown]",
            "passwd: files ]",
            "passwd: files \\\n",
            "passwd: files\nmalformed",
            "passwd: files\0",
            "passwd: fil\u{00e9}s",
            "\u{2000}passwd: files",
        ] {
            assert!(!passwd_enumeration_is_exhaustive(Ok(config)), "{config:?}");
        }
        let mut bounded = "passwd: files\n#".to_owned();
        bounded.extend(std::iter::repeat_n('x', MAX_NSSWITCH_BYTES - bounded.len()));
        assert!(passwd_enumeration_is_exhaustive(Ok(&bounded)));
        bounded.push('x');
        assert!(!passwd_enumeration_is_exhaustive(Ok(&bounded)));
        assert!(passwd_enumeration_is_exhaustive(Err(
            std::io::ErrorKind::NotFound
        )));
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::Other,
        ] {
            assert!(!passwd_enumeration_is_exhaustive(Err(kind)));
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
