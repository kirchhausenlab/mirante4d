//! Private transition checkpoints used by the B2 hostile-lifecycle evidence.
//!
//! Production builds execute the checkpoints as zero-state no-ops. Test
//! executables may select one exact transition, lane, edge, and occurrence
//! through the process environment. That keeps the fault seam out of the
//! public API while allowing both deterministic failure and VM power-cut
//! drivers to stop at the same named boundary.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;

#[cfg(test)]
use std::{
    env,
    io::{self, Write},
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

pub(crate) const READY_PREFIX: &str = "mirante4d-project-store-vm-ready:";
#[cfg(test)]
pub(crate) const TRACE_PREFIX: &str = "mirante4d-project-store-vm-trace:";
#[cfg(test)]
pub(crate) const HOSTED_HIT_PREFIX: &str = "mirante4d-project-store-hosted-hit:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionLane {
    None,
    Manual,
    Autosave,
}

impl TransitionLane {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Manual => "manual",
            Self::Autosave => "autosave",
        }
    }

    #[cfg(test)]
    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "manual" => Some(Self::Manual),
            "autosave" => Some(Self::Autosave),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionEdge {
    Before,
    After,
}

impl TransitionEdge {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }

    #[cfg(test)]
    fn parse(value: &str) -> Option<Self> {
        match value {
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum StoreTransition {
    MaintenanceLeaseAcquire,
    WriterLeaseTryAcquire,
    EnvelopeRead,
    RefRead,
    GenerationValidate,
    PayloadBindingValidate,
    ObjectInventory,
    WriterLeaseConfirm,
    ExpectedParentCheck,
    ObjectStageCreate,
    ObjectWrite,
    ObjectFileSync,
    ObjectPublishNoreplace,
    ObjectDirectorySync,
    GenerationStageCreate,
    GenerationWrite,
    GenerationFileSync,
    GenerationPublishNoreplace,
    GenerationDirectorySync,
    RecoveryStageCreate,
    RecoveryWrite,
    RecoveryFileSync,
    RecoveryReplace,
    RecoveryDirectorySync,
    HeadStageCreate,
    HeadWrite,
    HeadFileSync,
    HeadReplace,
    HeadDirectorySync,
    PackageStageCreate,
    ClosureCopyRehash,
    PackageTreeSync,
    PackageInstallNoreplace,
    DestinationParentSync,
    PinStageCreate,
    PinWrite,
    PinFileSync,
    PinReplace,
    PinDirectorySync,
    UnpinRemove,
    UnpinDirectorySync,
    StagingCleanupPayloadRemove,
    StagingCleanupDirectorySync,
    StagingCleanupTransactionRemove,
    GcMaintenanceUpgrade,
    GcRootScan,
    GcCandidateListing,
    GcTrashDirectoryCreate,
    GcTrashCollisionFileSync,
    GcTrashMove,
    GcActiveDeduplicateRemove,
    GcSourceDirectorySync,
    GcTrashDirectorySync,
    GcMaintenanceRestore,
    PurgeRemove,
    PurgeDirectorySync,
}

impl StoreTransition {
    pub(crate) const ALL: [Self; 56] = [
        Self::MaintenanceLeaseAcquire,
        Self::WriterLeaseTryAcquire,
        Self::EnvelopeRead,
        Self::RefRead,
        Self::GenerationValidate,
        Self::PayloadBindingValidate,
        Self::ObjectInventory,
        Self::WriterLeaseConfirm,
        Self::ExpectedParentCheck,
        Self::ObjectStageCreate,
        Self::ObjectWrite,
        Self::ObjectFileSync,
        Self::ObjectPublishNoreplace,
        Self::ObjectDirectorySync,
        Self::GenerationStageCreate,
        Self::GenerationWrite,
        Self::GenerationFileSync,
        Self::GenerationPublishNoreplace,
        Self::GenerationDirectorySync,
        Self::RecoveryStageCreate,
        Self::RecoveryWrite,
        Self::RecoveryFileSync,
        Self::RecoveryReplace,
        Self::RecoveryDirectorySync,
        Self::HeadStageCreate,
        Self::HeadWrite,
        Self::HeadFileSync,
        Self::HeadReplace,
        Self::HeadDirectorySync,
        Self::PackageStageCreate,
        Self::ClosureCopyRehash,
        Self::PackageTreeSync,
        Self::PackageInstallNoreplace,
        Self::DestinationParentSync,
        Self::PinStageCreate,
        Self::PinWrite,
        Self::PinFileSync,
        Self::PinReplace,
        Self::PinDirectorySync,
        Self::UnpinRemove,
        Self::UnpinDirectorySync,
        Self::StagingCleanupPayloadRemove,
        Self::StagingCleanupDirectorySync,
        Self::StagingCleanupTransactionRemove,
        Self::GcMaintenanceUpgrade,
        Self::GcRootScan,
        Self::GcCandidateListing,
        Self::GcTrashDirectoryCreate,
        Self::GcTrashCollisionFileSync,
        Self::GcTrashMove,
        Self::GcActiveDeduplicateRemove,
        Self::GcSourceDirectorySync,
        Self::GcTrashDirectorySync,
        Self::GcMaintenanceRestore,
        Self::PurgeRemove,
        Self::PurgeDirectorySync,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::MaintenanceLeaseAcquire => "maintenance_lease_acquire",
            Self::WriterLeaseTryAcquire => "writer_lease_try_acquire",
            Self::EnvelopeRead => "envelope_read",
            Self::RefRead => "ref_read",
            Self::GenerationValidate => "generation_validate",
            Self::PayloadBindingValidate => "payload_binding_validate",
            Self::ObjectInventory => "object_inventory",
            Self::WriterLeaseConfirm => "writer_lease_confirm",
            Self::ExpectedParentCheck => "expected_parent_check",
            Self::ObjectStageCreate => "object_stage_create",
            Self::ObjectWrite => "object_write",
            Self::ObjectFileSync => "object_file_sync",
            Self::ObjectPublishNoreplace => "object_publish_noreplace",
            Self::ObjectDirectorySync => "object_directory_sync",
            Self::GenerationStageCreate => "generation_stage_create",
            Self::GenerationWrite => "generation_write",
            Self::GenerationFileSync => "generation_file_sync",
            Self::GenerationPublishNoreplace => "generation_publish_noreplace",
            Self::GenerationDirectorySync => "generation_directory_sync",
            Self::RecoveryStageCreate => "recovery_stage_create",
            Self::RecoveryWrite => "recovery_write",
            Self::RecoveryFileSync => "recovery_file_sync",
            Self::RecoveryReplace => "recovery_replace",
            Self::RecoveryDirectorySync => "recovery_directory_sync",
            Self::HeadStageCreate => "head_stage_create",
            Self::HeadWrite => "head_write",
            Self::HeadFileSync => "head_file_sync",
            Self::HeadReplace => "head_replace",
            Self::HeadDirectorySync => "head_directory_sync",
            Self::PackageStageCreate => "package_stage_create",
            Self::ClosureCopyRehash => "closure_copy_rehash",
            Self::PackageTreeSync => "package_tree_sync",
            Self::PackageInstallNoreplace => "package_install_noreplace",
            Self::DestinationParentSync => "destination_parent_sync",
            Self::PinStageCreate => "pin_stage_create",
            Self::PinWrite => "pin_write",
            Self::PinFileSync => "pin_file_sync",
            Self::PinReplace => "pin_replace",
            Self::PinDirectorySync => "pin_directory_sync",
            Self::UnpinRemove => "unpin_remove",
            Self::UnpinDirectorySync => "unpin_directory_sync",
            Self::StagingCleanupPayloadRemove => "staging_cleanup_payload_remove",
            Self::StagingCleanupDirectorySync => "staging_cleanup_directory_sync",
            Self::StagingCleanupTransactionRemove => "staging_cleanup_transaction_remove",
            Self::GcMaintenanceUpgrade => "gc_maintenance_upgrade",
            Self::GcRootScan => "gc_root_scan",
            Self::GcCandidateListing => "gc_candidate_listing",
            Self::GcTrashDirectoryCreate => "gc_trash_directory_create",
            Self::GcTrashCollisionFileSync => "gc_trash_collision_file_sync",
            Self::GcTrashMove => "gc_trash_move",
            Self::GcActiveDeduplicateRemove => "gc_active_deduplicate_remove",
            Self::GcSourceDirectorySync => "gc_source_directory_sync",
            Self::GcTrashDirectorySync => "gc_trash_directory_sync",
            Self::GcMaintenanceRestore => "gc_maintenance_restore",
            Self::PurgeRemove => "purge_remove",
            Self::PurgeDirectorySync => "purge_directory_sync",
        }
    }

    #[cfg(test)]
    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|transition| transition.name() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TransitionPoint {
    transition: StoreTransition,
    lane: TransitionLane,
}

impl TransitionPoint {
    pub(crate) const fn new(transition: StoreTransition) -> Self {
        Self {
            transition,
            lane: TransitionLane::None,
        }
    }

    pub(crate) const fn in_lane(transition: StoreTransition, lane: TransitionLane) -> Self {
        Self { transition, lane }
    }

    pub(crate) const fn transition(self) -> StoreTransition {
        self.transition
    }

    pub(crate) const fn lane(self) -> TransitionLane {
        self.lane
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TransitionOccurrence {
    point: TransitionPoint,
    occurrence: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InjectedTransition;

impl fmt::Display for InjectedTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("injected project-store transition failure")
    }
}

impl std::error::Error for InjectedTransition {}

pub(crate) fn before(point: TransitionPoint) -> Result<TransitionOccurrence, InjectedTransition> {
    #[cfg(test)]
    {
        controller().before(point)
    }
    #[cfg(not(test))]
    {
        Ok(TransitionOccurrence {
            point,
            occurrence: 0,
        })
    }
}

pub(crate) fn after(occurrence: TransitionOccurrence) -> Result<(), InjectedTransition> {
    #[cfg(test)]
    {
        controller().after(occurrence)
    }
    #[cfg(not(test))]
    {
        let _ = occurrence;
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransitionAction {
    Fail,
    Park,
    BracketPark,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TransitionTarget {
    point: TransitionPoint,
    edge: TransitionEdge,
    occurrence: usize,
    action: TransitionAction,
}

#[cfg(test)]
struct TransitionController {
    target: Option<TransitionTarget>,
    trace: bool,
    attempts: [AtomicUsize; 56 * 3],
}

#[cfg(test)]
impl TransitionController {
    fn from_environment() -> Self {
        let role = env::var("MIRANTE4D_PROJECT_STORE_VM_ROLE").ok();
        let transition = (role.as_deref() == Some("exercise"))
            .then(|| {
                env::var("MIRANTE4D_PROJECT_STORE_HOSTED_TRANSITION")
                    .or_else(|_| env::var("MIRANTE4D_PROJECT_STORE_VM_TRANSITION"))
                    .ok()
            })
            .flatten();
        let target = transition.map(|transition| {
            let transition = StoreTransition::parse(&transition)
                .unwrap_or_else(|| panic!("unknown project-store transition {transition:?}"));
            let lane = env::var("MIRANTE4D_PROJECT_STORE_HOSTED_LANE")
                .or_else(|_| env::var("MIRANTE4D_PROJECT_STORE_VM_LANE"))
                .ok()
                .as_deref()
                .and_then(TransitionLane::parse)
                .unwrap_or(TransitionLane::None);
            let edge = env::var("MIRANTE4D_PROJECT_STORE_HOSTED_EDGE")
                .or_else(|_| env::var("MIRANTE4D_PROJECT_STORE_VM_EDGE"))
                .ok()
                .as_deref()
                .and_then(TransitionEdge::parse)
                .unwrap_or(TransitionEdge::After);
            let occurrence = env::var("MIRANTE4D_PROJECT_STORE_HOSTED_OCCURRENCE")
                .or_else(|_| env::var("MIRANTE4D_PROJECT_STORE_VM_OCCURRENCE"))
                .ok()
                .map(|value| {
                    value
                        .parse::<usize>()
                        .unwrap_or_else(|_| panic!("invalid transition occurrence {value:?}"))
                })
                .unwrap_or(0);
            let action = match env::var("MIRANTE4D_PROJECT_STORE_TRANSITION_ACTION").as_deref() {
                Ok("fail") => TransitionAction::Fail,
                Ok("park") | Err(_) => TransitionAction::Park,
                Ok("bracket-park") => TransitionAction::BracketPark,
                Ok(other) => panic!("unknown transition action {other:?}"),
            };
            TransitionTarget {
                point: TransitionPoint::in_lane(transition, lane),
                edge,
                occurrence,
                action,
            }
        });
        Self {
            target,
            trace: role.as_deref() == Some("trace"),
            attempts: [const { AtomicUsize::new(0) }; 56 * 3],
        }
    }

    fn before(&self, point: TransitionPoint) -> Result<TransitionOccurrence, InjectedTransition> {
        let occurrence = self.attempts[point.transition() as usize * 3 + point.lane() as usize]
            .fetch_add(1, Ordering::AcqRel);
        let occurrence = TransitionOccurrence { point, occurrence };
        self.hit(occurrence, TransitionEdge::Before)?;
        Ok(occurrence)
    }

    fn after(&self, occurrence: TransitionOccurrence) -> Result<(), InjectedTransition> {
        self.hit(occurrence, TransitionEdge::After)
    }

    fn hit(
        &self,
        occurrence: TransitionOccurrence,
        edge: TransitionEdge,
    ) -> Result<(), InjectedTransition> {
        if self.trace {
            println!(
                "{TRACE_PREFIX}{{\"schema\":\"mirante4d-wp10b-vm-transition-trace\",\"schema_version\":1,\"transition\":\"{}\",\"lane\":\"{}\",\"edge\":\"{}\",\"occurrence\":{}}}",
                occurrence.point.transition().name(),
                occurrence.point.lane().name(),
                edge.name(),
                occurrence.occurrence
            );
            io::stdout()
                .flush()
                .expect("flush project-store VM transition trace");
        }
        let Some(target) = self.target else {
            return Ok(());
        };
        if target.point != occurrence.point || target.occurrence != occurrence.occurrence {
            return Ok(());
        }
        if target.action == TransitionAction::BracketPark {
            self.bracket_park(occurrence, edge);
            return Ok(());
        }
        if target.edge != edge {
            return Ok(());
        }
        match target.action {
            TransitionAction::Fail => {
                emit_hosted_hit(occurrence, edge, "failed");
                Err(InjectedTransition)
            }
            TransitionAction::Park => {
                let case = env::var("MIRANTE4D_PROJECT_STORE_VM_CASE")
                    .unwrap_or_else(|_| "unspecified".to_owned());
                assert!(
                    !case.is_empty()
                        && case
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
                    "project-store VM case ID is not report-safe"
                );
                println!(
                    "{READY_PREFIX}{{\"schema\":\"mirante4d-wp10b-vm-transition-marker\",\"schema_version\":1,\"role\":\"exercise\",\"case\":\"{}\",\"transition\":\"{}\",\"lane\":\"{}\",\"edge\":\"{}\",\"occurrence\":{},\"status\":\"ready\"}}",
                    case,
                    occurrence.point.transition().name(),
                    occurrence.point.lane().name(),
                    edge.name(),
                    occurrence.occurrence
                );
                io::stdout()
                    .flush()
                    .expect("flush project-store VM transition marker");
                loop {
                    thread::park_timeout(Duration::from_secs(60));
                }
            }
            TransitionAction::BracketPark => unreachable!("handled above"),
        }
    }

    fn bracket_park(&self, occurrence: TransitionOccurrence, edge: TransitionEdge) {
        use std::{ffi::OsString, fs, path::PathBuf};

        emit_hosted_hit(occurrence, edge, "parked");
        let base = env::var_os("MIRANTE4D_PROJECT_STORE_TRANSITION_RELEASE")
            .expect("bracket-park requires a release path");
        let mut release: OsString = base;
        release.push(".");
        release.push(edge.name());
        let release = PathBuf::from(release);
        while !release.exists() {
            thread::park_timeout(Duration::from_millis(1));
        }
        fs::remove_file(&release).expect("remove consumed bracket release marker");
    }
}

#[cfg(test)]
fn emit_hosted_hit(occurrence: TransitionOccurrence, edge: TransitionEdge, status: &str) {
    let case =
        env::var("MIRANTE4D_PROJECT_STORE_VM_CASE").unwrap_or_else(|_| "unspecified".to_owned());
    assert!(
        !case.is_empty()
            && case
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
        "project-store hosted case ID is not report-safe"
    );
    println!(
        "{HOSTED_HIT_PREFIX}{{\"schema\":\"mirante4d-wp10b-hosted-transition-hit\",\"schema_version\":1,\"case\":\"{}\",\"transition\":\"{}\",\"lane\":\"{}\",\"edge\":\"{}\",\"occurrence\":{},\"status\":\"{}\"}}",
        case,
        occurrence.point.transition().name(),
        occurrence.point.lane().name(),
        edge.name(),
        occurrence.occurrence,
        status
    );
    io::stdout()
        .flush()
        .expect("flush project-store hosted transition marker");
}

#[cfg(test)]
fn controller() -> &'static TransitionController {
    static CONTROLLER: OnceLock<TransitionController> = OnceLock::new();
    CONTROLLER.get_or_init(TransitionController::from_environment)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::{
        StoreTransition, TransitionAction, TransitionController, TransitionEdge, TransitionLane,
        TransitionPoint, TransitionTarget,
    };

    #[test]
    fn transition_inventory_names_are_unique_and_round_trip() {
        let mut names = StoreTransition::ALL
            .into_iter()
            .map(StoreTransition::name)
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 56);
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 56);
        for transition in StoreTransition::ALL {
            assert_eq!(StoreTransition::parse(transition.name()), Some(transition));
        }
        assert_eq!(
            TransitionLane::parse("manual"),
            Some(TransitionLane::Manual)
        );
        assert_eq!(
            TransitionLane::parse("autosave"),
            Some(TransitionLane::Autosave)
        );
        assert_eq!(TransitionLane::parse("none"), Some(TransitionLane::None));
    }

    #[test]
    fn every_transition_edge_and_repeated_occurrence_is_injectable() {
        for transition in StoreTransition::ALL {
            let lanes: &[_] = if matches!(
                transition,
                StoreTransition::RecoveryStageCreate
                    | StoreTransition::RecoveryWrite
                    | StoreTransition::RecoveryFileSync
                    | StoreTransition::RecoveryReplace
                    | StoreTransition::RecoveryDirectorySync
                    | StoreTransition::HeadStageCreate
                    | StoreTransition::HeadWrite
                    | StoreTransition::HeadFileSync
                    | StoreTransition::HeadReplace
                    | StoreTransition::HeadDirectorySync
            ) {
                &[TransitionLane::Manual, TransitionLane::Autosave]
            } else {
                &[TransitionLane::None]
            };
            for lane in lanes {
                for edge in [TransitionEdge::Before, TransitionEdge::After] {
                    let point = TransitionPoint::in_lane(transition, *lane);
                    let controller = TransitionController {
                        target: Some(TransitionTarget {
                            point,
                            edge,
                            occurrence: 1,
                            action: TransitionAction::Fail,
                        }),
                        trace: false,
                        attempts: [const { AtomicUsize::new(0) }; 56 * 3],
                    };
                    let first = controller.before(point).unwrap();
                    assert!(controller.after(first).is_ok());
                    match edge {
                        TransitionEdge::Before => {
                            assert!(controller.before(point).is_err());
                        }
                        TransitionEdge::After => {
                            let second = controller.before(point).unwrap();
                            assert!(controller.after(second).is_err());
                        }
                    }
                }
            }
        }
    }
}
