//! Working storage for a native run; selected before runtime initialization.
use std::sync::OnceLock;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingStorage {
    Memory,
    Disk,
}
static POLICY: OnceLock<WorkingStorage> = OnceLock::new();
pub(crate) fn select(policy: WorkingStorage) {
    assert_eq!(
        *POLICY.get_or_init(|| policy),
        policy,
        "working storage already initialized"
    );
}
pub(crate) fn current() -> WorkingStorage {
    *POLICY.get_or_init(|| WorkingStorage::Disk)
}
