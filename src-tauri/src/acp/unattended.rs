//! How a connection nobody is watching answers a permission request.
//!
//! A foreground conversation parks its approval card for as long as it takes
//! a person to look at it. A broker-owned delegation child or a work-task run
//! has no person: on 2026-10-31 six delegated children hit approval cards
//! (`ls | grep`, a multi-line `python3 -c`, a cross-workspace `find`), sat
//! blocked until their coordinator noticed and canceled them, and the next
//! round was re-delegated with "only use Read/Write/Edit this round" — a
//! denial the child could have adapted to minutes earlier on its own.
//!
//! The fix keeps the card (a person watching the child session can still
//! answer it) and adds a grace timer: if nobody answers within `grace`, the
//! policy decides. The decision itself reuses [`crate::cli_run::policy`] —
//! same option picking by kind, never an `*_always` that would write a
//! standing rule into the user's agent settings.
//!
//! Scope is decided at spawn ([`resolve`]): ONLY broker-owned delegation
//! children (`delegation_task_id`) and work-task runs (owner label
//! `work_task`) are unattended. A user's own tab keeps today's
//! wait-forever behavior, as does a `codeg-cli` run's ROOT — its own
//! [`crate::cli_run::policy`] answers. A delegation child that inherited a
//! `CliRunTag` gets both answerers: the run's policy answers first, and the
//! grace timer is a backstop for a `wait`-policy run that would otherwise
//! hang the child exactly like an unwatched board card.

use std::time::Duration;

/// Env key that overrides the on-timeout decision: `deny` (default) or
/// `reads` (let read/search kinds through, refuse the rest). `off` disables
/// unattended handling entirely. Values are [`crate::cli_run::policy`]'s own
/// names, minus the ones that make no sense unattended (`allow` — a blank
/// check nobody asked for; `fail` — there is no run to fail; `wait` — that
/// IS the disabled state).
pub const POLICY_ENV: &str = "CODEG_UNATTENDED_PERMISSION";

/// Env key for how long a card stays up for a person before the policy
/// decides. Seconds; default [`DEFAULT_GRACE`].
pub const GRACE_ENV: &str = "CODEG_UNATTENDED_PERMISSION_GRACE_SECS";

/// Default grace before the policy answers. Long enough for a person
/// watching the child session to answer; short enough that a fan-out of
/// blocked children does not sit dead for a coordinator's whole polling
/// cycle (the 2026-10-31 children waited 1–4 minutes before being canceled).
pub const DEFAULT_GRACE: Duration = Duration::from_secs(120);

/// The owner label the work-task engine spawns its executors under.
pub const WORK_TASK_OWNER_LABEL: &str = "work_task";

/// What an unattended connection does with a permission request nobody
/// answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnattendedPermissions {
    /// Decision applied when the grace runs out. Semantics are
    /// [`crate::cli_run::policy::PermissionPolicy`]'s; only `deny` and
    /// `reads` are meaningful here (see [`POLICY_ENV`]).
    pub on_timeout: crate::cli_run::policy::PermissionPolicy,
    /// How long the card stays answerable by a person first.
    pub grace: Duration,
}

/// Decide whether a spawn is unattended, and with what policy.
///
/// Precedence:
/// 1. `CODEG_UNATTENDED_PERMISSION=off` → `None` (wait forever, as today).
/// 2. A connection that is nobody's unattended work — no `delegation_task_id`
///    and not the work-task owner label — → `None`, whatever the env says:
///    the env tunes the unattended path, it must never make a person's tab
///    auto-deny.
/// 3. A `codeg-cli`-rooted connection → `None`: the run's own policy (or an
///    inherited `CliRunTag` on a child) answers through the run machinery.
/// 4. Otherwise `Some` with the env's decision (default `deny`) and the env's
///    grace (default [`DEFAULT_GRACE`]).
pub fn resolve(
    owner_window_label: &str,
    delegation_task_id: Option<&str>,
    cli_run_root: bool,
    env: &std::collections::BTreeMap<String, String>,
) -> Option<UnattendedPermissions> {
    if cli_run_root {
        return None;
    }
    let unattended = delegation_task_id.is_some() || owner_window_label == WORK_TASK_OWNER_LABEL;
    if !unattended {
        return None;
    }
    let raw = env.get(POLICY_ENV).map(String::as_str).unwrap_or("");
    if raw.eq_ignore_ascii_case("off") {
        return None;
    }
    let on_timeout = match raw.trim() {
        "" => crate::cli_run::policy::PermissionPolicy::Deny,
        // `parse` accepts every CLI name; the nonsensical ones (`allow`,
        // `fail`, `wait`) fall through to the default rather than being
        // honored — `wait` especially, which would silently restore the
        // hang this module exists to end.
        other => crate::cli_run::policy::PermissionPolicy::parse(other)
            .filter(|p| {
                matches!(
                    p,
                    crate::cli_run::policy::PermissionPolicy::Deny
                        | crate::cli_run::policy::PermissionPolicy::Reads
                )
            })
            .unwrap_or(crate::cli_run::policy::PermissionPolicy::Deny),
    };
    let grace = env
        .get(GRACE_ENV)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_GRACE);
    Some(UnattendedPermissions { on_timeout, grace })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_persons_tab_is_never_unattended_even_with_the_env_set() {
        assert_eq!(resolve("main", None, false, &env(&[(POLICY_ENV, "deny")]),), None);
        assert_eq!(resolve("main", None, false, &env(&[(POLICY_ENV, "reads")])), None);
    }

    #[test]
    fn delegation_children_and_work_task_runs_default_to_deny_with_grace() {
        let default = resolve("main", Some("task-1"), false, &BTreeMap::new()).unwrap();
        assert_eq!(
            default.on_timeout,
            crate::cli_run::policy::PermissionPolicy::Deny
        );
        assert_eq!(default.grace, DEFAULT_GRACE);

        let board = resolve(WORK_TASK_OWNER_LABEL, None, false, &BTreeMap::new()).unwrap();
        assert_eq!(board, default);
    }

    #[test]
    fn off_disables_and_only_deny_and_reads_are_honored() {
        assert_eq!(resolve("main", Some("t"), false, &env(&[(POLICY_ENV, "off")])), None);
        assert_eq!(resolve("main", Some("t"), false, &env(&[(POLICY_ENV, "OFF")])), None);
        for (raw, expect_reads) in [("reads", true), ("allow", false), ("wait", false), ("yolo", false)] {
            let got = resolve("main", Some("t"), false, &env(&[(POLICY_ENV, raw)]))
                .expect("unattended stays enabled");
            assert_eq!(
                got.on_timeout == crate::cli_run::policy::PermissionPolicy::Reads,
                expect_reads,
                "{raw}"
            );
        }
    }

    #[test]
    fn grace_env_needs_positive_seconds() {
        let got = resolve("main", Some("t"), false, &env(&[(GRACE_ENV, "30")])).unwrap();
        assert_eq!(got.grace, Duration::from_secs(30));
        for bad in ["0", "-5", "not-a-number"] {
            let got = resolve("main", Some("t"), false, &env(&[(GRACE_ENV, bad)])).unwrap();
            assert_eq!(got.grace, DEFAULT_GRACE, "{bad}");
        }
    }

    #[test]
    fn a_cli_run_root_answers_by_its_own_policy_instead() {
        assert_eq!(resolve("main", None, true, &BTreeMap::new()), None);
        // Even a delegation child that IS a cli root's child carries a
        // CliRunTag; the run answers it. (Roots pass cli_run_root=true;
        // children are marked by inheritance, not by this call.)
        assert_eq!(
            resolve(WORK_TASK_OWNER_LABEL, Some("t"), true, &BTreeMap::new()),
            None
        );
    }
}
