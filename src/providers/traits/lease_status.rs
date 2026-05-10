use std::fmt;

/// Diagnostic label for a lease, used in log/tracing output.
///
/// Each lease type controls its own formatting so it can include
/// whichever fields are most useful for human readers — e.g.
/// `project=my-proj-123` or `account=acct-456, email=foo@bar.com`.
pub trait LeaseLabel {
    fn fmt_label(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

/// Result of evaluating a single credential candidate for a given model.
#[derive(Debug)]
pub(crate) enum LeaseStatus<L> {
    /// Credential is usable — here is the lease.
    Ready(L),
    /// Credential has expired and needs refreshing.
    Expired,
    /// Credential is in a rate-limit cooldown for this model.
    Cooling,
    /// Credential is already being refreshed.
    Refreshing,
    /// Credential does not support the requested model.
    Unsupported,
    /// Credential ID not found in the manager.
    Missing,
}

impl<L: LeaseLabel> fmt::Display for LeaseStatus<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseStatus::Ready(lease) => {
                f.write_str("ready(")?;
                lease.fmt_label(f)?;
                f.write_str(")")
            }
            LeaseStatus::Expired => f.write_str("expired"),
            LeaseStatus::Cooling => f.write_str("cooling"),
            LeaseStatus::Refreshing => f.write_str("refreshing"),
            LeaseStatus::Unsupported => f.write_str("unsupported"),
            LeaseStatus::Missing => f.write_str("missing"),
        }
    }
}
