//! Shared bounded read-side validation for established project stores.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use mirante4d_identity::{ExactBytesDigest, RawObjectDescriptor};
use mirante4d_project_model::ProjectId;

use crate::{
    ProjectGenerationId, ProjectOpenMode, ProjectStoreFault, ProjectStoreLimits, ProjectStorePath,
    generation::{
        ArtifactStorage, GenerationDocument, GenerationKind, LogicalObjectBinding, PhysicalObject,
    },
    lease::{LeaseError, ProjectStoreLeases},
    local::{LocalPublicationError, LocalStoreRoot},
    wire::{RefKind, RefRecord},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LaneSnapshot {
    pub(crate) head: RefRecord,
    pub(crate) recovery: Option<RefRecord>,
}

pub(crate) struct EstablishedStoreInspection {
    project_id: ProjectId,
    manual: LaneSnapshot,
    autosave: Option<LaneSnapshot>,
    manual_generation: GenerationDocument,
    autosave_generation: Option<GenerationDocument>,
    maximum_generation_sequence: u64,
    maximum_revision_high_water: u64,
}

/// One healthy store state before an established/provisional policy is chosen.
/// It owns no path and never repairs refs.
pub(crate) struct StoreStateInspection {
    project_id: ProjectId,
    manual: Option<LaneSnapshot>,
    autosave: Option<LaneSnapshot>,
    manual_generation: Option<GenerationDocument>,
    autosave_generation: Option<GenerationDocument>,
    pins: Vec<RefRecord>,
    maximum_generation_sequence: u64,
    maximum_revision_high_water: u64,
}

impl StoreStateInspection {
    pub(crate) const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub(crate) const fn is_provisional(&self) -> bool {
        self.manual.is_none()
    }

    pub(crate) const fn manual(&self) -> Option<LaneSnapshot> {
        self.manual
    }

    pub(crate) const fn autosave(&self) -> Option<LaneSnapshot> {
        self.autosave
    }

    pub(crate) fn manual_generation(&self) -> Option<&GenerationDocument> {
        self.manual_generation.as_ref()
    }

    pub(crate) fn autosave_generation(&self) -> Option<&GenerationDocument> {
        self.autosave_generation.as_ref()
    }

    pub(crate) fn authority_generation(&self) -> &GenerationDocument {
        self.manual_generation
            .as_ref()
            .or(self.autosave_generation.as_ref())
            .expect("a healthy store has one authoritative lane")
    }

    fn root_generation_ids(&self) -> BTreeSet<ProjectGenerationId> {
        let mut roots = BTreeSet::new();
        for lane in [self.manual, self.autosave].into_iter().flatten() {
            roots.insert(lane.head.current());
            roots.extend(lane.head.previous());
            roots.extend(lane.recovery.map(RefRecord::current));
        }
        roots.extend(self.pins.iter().map(|pin| pin.current()));
        roots
    }
}

/// One bounded metadata graph for recovery review and later compaction. Bulk
/// payload bytes are not hashed by this read-side snapshot.
pub(crate) struct StoreGraphInspection {
    state: StoreStateInspection,
    generation_ids: Vec<ProjectGenerationId>,
    root_generation_ids: Vec<ProjectGenerationId>,
    orphan_generation_ids: Vec<ProjectGenerationId>,
    object_facts: Vec<PhysicalObject>,
    live_object_facts: Vec<PhysicalObject>,
    unrooted_object_facts: Vec<PhysicalObject>,
}

impl StoreGraphInspection {
    pub(crate) const fn state(&self) -> &StoreStateInspection {
        &self.state
    }

    pub(crate) fn generation_ids(&self) -> &[ProjectGenerationId] {
        &self.generation_ids
    }

    pub(crate) fn root_generation_ids(&self) -> &[ProjectGenerationId] {
        &self.root_generation_ids
    }

    pub(crate) fn orphan_generation_ids(&self) -> &[ProjectGenerationId] {
        &self.orphan_generation_ids
    }

    pub(crate) fn object_facts(&self) -> &[PhysicalObject] {
        &self.object_facts
    }

    pub(crate) fn live_object_facts(&self) -> &[PhysicalObject] {
        &self.live_object_facts
    }

    /// Diagnostic partition only. This is not a trash plan because an object
    /// may still be shared by an unselected recovery candidate.
    pub(crate) fn unrooted_object_facts(&self) -> &[PhysicalObject] {
        &self.unrooted_object_facts
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EstablishedAutosaveClassification {
    Newer,
    Stale,
    Divergent,
}

impl EstablishedStoreInspection {
    pub(crate) const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub(crate) const fn manual(&self) -> LaneSnapshot {
        self.manual
    }

    pub(crate) const fn autosave(&self) -> Option<LaneSnapshot> {
        self.autosave
    }

    pub(crate) fn manual_generation(&self) -> &GenerationDocument {
        &self.manual_generation
    }

    pub(crate) fn autosave_generation(&self) -> Option<&GenerationDocument> {
        self.autosave_generation.as_ref()
    }

    pub(crate) fn autosave_classification(&self) -> Option<EstablishedAutosaveClassification> {
        let autosave = self.autosave?;
        let generation = self.autosave_generation.as_ref()?;
        if autosave.head.base() != Some(self.manual.head.current()) {
            Some(EstablishedAutosaveClassification::Divergent)
        } else if generation.projection().revision()
            == self.manual_generation.projection().revision()
        {
            Some(EstablishedAutosaveClassification::Stale)
        } else {
            Some(EstablishedAutosaveClassification::Newer)
        }
    }

    pub(crate) const fn maximum_revision_high_water(&self) -> u64 {
        self.maximum_revision_high_water
    }

    pub(crate) fn next_generation_sequence(&self) -> Result<u64, ProjectStoreFault> {
        self.maximum_generation_sequence
            .checked_add(1)
            .ok_or(ProjectStoreFault::Capacity {
                stage: "generation_sequence",
            })
    }
}

pub(crate) struct OpenedEstablishedStore {
    root: LocalStoreRoot,
    leases: ProjectStoreLeases,
    inspection: EstablishedStoreInspection,
}

impl OpenedEstablishedStore {
    pub(crate) const fn effective_mode(&self) -> ProjectOpenMode {
        self.leases.effective_mode()
    }

    pub(crate) const fn inspection(&self) -> &EstablishedStoreInspection {
        &self.inspection
    }

    pub(crate) fn into_resources(self) -> (LocalStoreRoot, ProjectStoreLeases) {
        (self.root, self.leases)
    }
}

pub(crate) fn open_established_store<C>(
    path: &ProjectStorePath,
    requested_mode: ProjectOpenMode,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<OpenedEstablishedStore, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    check_cancelled(&mut is_cancelled)?;
    let root =
        LocalStoreRoot::open(path.as_path()).map_err(|error| map_local_error(error, "open"))?;
    let leases = ProjectStoreLeases::acquire(&root, requested_mode).map_err(map_lease_error)?;
    let inspection = inspect_established_store(&root, limits, &mut is_cancelled)?;
    Ok(OpenedEstablishedStore {
        root,
        leases,
        inspection,
    })
}

pub(crate) fn inspect_established_store<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    is_cancelled: C,
) -> Result<EstablishedStoreInspection, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let state = inspect_store_state(root, limits, is_cancelled)?;
    let manual = state.manual.ok_or(ProjectStoreFault::Corruption {
        stage: "manual_head",
    })?;
    let manual_generation = state
        .manual_generation
        .ok_or(ProjectStoreFault::Corruption {
            stage: "manual_generation",
        })?;
    Ok(EstablishedStoreInspection {
        project_id: state.project_id,
        manual,
        autosave: state.autosave,
        manual_generation,
        autosave_generation: state.autosave_generation,
        maximum_generation_sequence: state.maximum_generation_sequence,
        maximum_revision_high_water: state.maximum_revision_high_water,
    })
}

pub(crate) fn inspect_store_state<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<StoreStateInspection, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    check_cancelled(&mut is_cancelled)?;
    root.validate_store_inventory(limits, 0, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    let envelope = root
        .read_project_envelope(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "project_envelope"))?;
    let project_id = envelope.project_id();
    let manual = read_lane_snapshot(
        root,
        project_id,
        RefKind::ManualHead,
        RefKind::ManualRecovery,
        limits,
        &mut is_cancelled,
    )?;
    let autosave = read_lane_snapshot(
        root,
        project_id,
        RefKind::AutosaveHead,
        RefKind::AutosaveRecovery,
        limits,
        &mut is_cancelled,
    )?;
    let pins = root
        .read_pin_refs(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "ref_namespace"))?;
    if pins.iter().any(|pin| pin.project_id() != project_id) {
        return Err(ProjectStoreFault::Corruption {
            stage: "pin_identity",
        });
    }

    let manual_generation = manual
        .map(|lane| {
            read_current_generation(
                root,
                project_id,
                lane,
                GenerationKind::Manual,
                limits,
                &mut is_cancelled,
                "manual_generation",
            )
        })
        .transpose()?;
    let autosave_generation = if let Some(autosave_lane) = autosave {
        let generation = read_current_generation(
            root,
            project_id,
            autosave_lane,
            GenerationKind::Autosave,
            limits,
            &mut is_cancelled,
            "autosave_generation",
        )?;
        Some(generation)
    } else {
        None
    };

    match (manual_generation.as_ref(), autosave_generation.as_ref()) {
        (None, None) => {
            return Err(ProjectStoreFault::Corruption {
                stage: "store_state",
            });
        }
        (None, Some(autosave_generation)) => {
            if autosave.is_none_or(|lane| lane.head.base().is_some())
                || autosave_generation.base_manual_generation_id().is_some()
            {
                return Err(ProjectStoreFault::Corruption {
                    stage: "provisional_state",
                });
            }
        }
        (Some(_), None) => {}
        (Some(manual_generation), Some(autosave_generation)) => {
            if autosave.is_none_or(|lane| lane.head.base().is_none())
                || autosave_generation.forked_from() != manual_generation.forked_from()
                || !autosave_generation
                    .projection()
                    .state()
                    .dataset()
                    .has_same_scientific_content(manual_generation.projection().state().dataset())
            {
                return Err(ProjectStoreFault::Corruption {
                    stage: "autosave_generation_continuity",
                });
            }
        }
    }

    validate_referenced_generations(
        root,
        project_id,
        manual,
        autosave,
        &pins,
        manual_generation.as_ref(),
        autosave_generation.as_ref(),
        limits,
        &mut is_cancelled,
    )?;
    let current_generations = manual_generation.iter().chain(autosave_generation.iter());
    let maximum_generation_sequence = current_generations
        .clone()
        .map(GenerationDocument::generation_sequence)
        .max()
        .expect("a healthy store has one current generation");
    let maximum_revision_high_water = current_generations
        .map(|generation| generation.projection().revision_high_water().sequence())
        .max()
        .expect("a healthy store has one current generation");
    Ok(StoreStateInspection {
        project_id,
        manual,
        autosave,
        manual_generation,
        autosave_generation,
        pins,
        maximum_generation_sequence,
        maximum_revision_high_water,
    })
}

pub(crate) fn inspect_store_graph<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<StoreGraphInspection, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    let state = inspect_store_state(root, limits, &mut is_cancelled)?;
    let generation_ids = root
        .enumerate_generation_ids(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "generation_namespace"))?;
    let generation_set = generation_ids.iter().copied().collect::<BTreeSet<_>>();
    let root_set = state.root_generation_ids();
    if !root_set.is_subset(&generation_set) {
        return Err(ProjectStoreFault::Corruption {
            stage: "generation_namespace",
        });
    }
    let orphan_generation_ids = generation_set
        .difference(&root_set)
        .copied()
        .collect::<Vec<_>>();
    if orphan_generation_ids.len() > limits.recovery_candidates_max {
        return Err(ProjectStoreFault::Capacity {
            stage: "recovery_candidates",
        });
    }
    let object_facts = root
        .enumerate_object_facts(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "object_namespace"))?
        .into_iter()
        .map(|(digest, byte_length)| PhysicalObject::new(digest, byte_length))
        .collect::<Vec<_>>();
    let actual_objects = object_facts
        .iter()
        .map(|object| (object.digest(), object.byte_length()))
        .collect::<BTreeMap<_, _>>();
    let authority = state.authority_generation();
    let mut live_objects = BTreeSet::new();
    let mut provenance = BTreeMap::new();
    for generation_id in &generation_ids {
        check_cancelled(&mut is_cancelled)?;
        let bytes = root
            .read_generation_bytes(
                *generation_id,
                limits.generation_bytes_max,
                &mut is_cancelled,
            )
            .map_err(|error| map_local_error(error, "generation_graph"))?;
        let document =
            GenerationDocument::decode(*generation_id, state.project_id(), &bytes, limits)
                .map_err(|_| ProjectStoreFault::Corruption {
                    stage: "generation_graph",
                })?;
        if document.forked_from() != authority.forked_from()
            || !document
                .projection()
                .state()
                .dataset()
                .has_same_scientific_content(authority.projection().state().dataset())
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "generation_graph",
            });
        }
        provenance.insert(
            *generation_id,
            (
                document.kind(),
                document.parent_generation_id(),
                document.base_manual_generation_id(),
            ),
        );
        validate_physical_closure_metadata(root, &document, limits, &mut is_cancelled)?;
        for object in document.reachable_objects() {
            if actual_objects.get(&object.digest()) != Some(&object.byte_length()) {
                return Err(ProjectStoreFault::Corruption {
                    stage: "object_namespace",
                });
            }
            if root_set.contains(generation_id) {
                live_objects.insert(*object);
            }
        }
    }
    for (kind, parent, base) in provenance.values() {
        if parent.is_some_and(|parent| {
            provenance
                .get(&parent)
                .is_some_and(|(parent_kind, _, _)| parent_kind != kind)
        }) || base.is_some_and(|base| {
            provenance
                .get(&base)
                .is_some_and(|(base_kind, _, _)| *base_kind != GenerationKind::Manual)
        }) {
            return Err(ProjectStoreFault::Corruption {
                stage: "generation_provenance",
            });
        }
    }

    let root_generation_ids = root_set.iter().copied().collect::<Vec<_>>();
    let live_object_facts = live_objects.iter().copied().collect::<Vec<_>>();
    let unrooted_object_facts = object_facts
        .iter()
        .copied()
        .filter(|object| !live_objects.contains(object))
        .collect::<Vec<_>>();
    Ok(StoreGraphInspection {
        state,
        generation_ids,
        root_generation_ids,
        orphan_generation_ids,
        object_facts,
        live_object_facts,
        unrooted_object_facts,
    })
}

pub(crate) fn read_lane_snapshot<C>(
    root: &LocalStoreRoot,
    project_id: ProjectId,
    head_kind: RefKind,
    recovery_kind: RefKind,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<Option<LaneSnapshot>, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let head = root
        .read_ref(head_kind, limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "lane_head"))?;
    let recovery = root
        .read_ref(recovery_kind, limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "lane_recovery"))?;
    let Some(head) = head else {
        if recovery.is_some() {
            return Err(ProjectStoreFault::Corruption {
                stage: "recovery_without_head",
            });
        }
        return Ok(None);
    };
    if head.project_id() != project_id
        || head.previous() == Some(head.current())
        || recovery.is_some_and(|record| record.project_id() != project_id)
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "lane_ref_identity",
        });
    }
    let pair_valid = match recovery {
        None => head.previous().is_none(),
        Some(record) => {
            record.current() == head.current() || Some(record.current()) == head.previous()
        }
    };
    if !pair_valid {
        return Err(ProjectStoreFault::Corruption {
            stage: "recovery_pair",
        });
    }
    Ok(Some(LaneSnapshot { head, recovery }))
}

fn read_current_generation<C>(
    root: &LocalStoreRoot,
    project_id: ProjectId,
    lane: LaneSnapshot,
    kind: GenerationKind,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    stage: &'static str,
) -> Result<GenerationDocument, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let bytes = root
        .read_generation_bytes(
            lane.head.current(),
            limits.generation_bytes_max,
            &mut *is_cancelled,
        )
        .map_err(|error| map_local_error(error, stage))?;
    let document = GenerationDocument::decode(lane.head.current(), project_id, &bytes, limits)
        .map_err(|_| ProjectStoreFault::Corruption { stage })?;
    if document.kind() != kind
        || document.parent_generation_id() != lane.head.previous()
        || document.base_manual_generation_id() != lane.head.base()
    {
        return Err(ProjectStoreFault::Corruption { stage });
    }
    validate_physical_closure_metadata(root, &document, limits, &mut *is_cancelled)?;
    Ok(document)
}

#[allow(clippy::too_many_arguments)]
fn validate_referenced_generations<C>(
    root: &LocalStoreRoot,
    project_id: ProjectId,
    manual: Option<LaneSnapshot>,
    autosave: Option<LaneSnapshot>,
    pins: &[RefRecord],
    manual_current: Option<&GenerationDocument>,
    autosave_current: Option<&GenerationDocument>,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let mut targets = BTreeMap::<ProjectGenerationId, Option<GenerationKind>>::new();
    if let Some(manual) = manual {
        for id in [
            Some(manual.head.current()),
            manual.head.previous(),
            manual.recovery.map(RefRecord::current),
        ]
        .into_iter()
        .flatten()
        {
            insert_expected_generation(&mut targets, id, Some(GenerationKind::Manual))?;
        }
    }
    if let Some(autosave) = autosave {
        for id in [
            Some(autosave.head.current()),
            autosave.head.previous(),
            autosave.recovery.map(RefRecord::current),
        ]
        .into_iter()
        .flatten()
        {
            insert_expected_generation(&mut targets, id, Some(GenerationKind::Autosave))?;
        }
    }
    for pin in pins {
        insert_expected_generation(&mut targets, pin.current(), None)?;
    }

    let manual_current_id = manual.map(|lane| lane.head.current());
    let autosave_current_id = autosave.map(|lane| lane.head.current());
    let authority = manual_current
        .or(autosave_current)
        .expect("a healthy store has one authoritative generation");
    for (id, expected_kind) in targets {
        if Some(id) == manual_current_id || Some(id) == autosave_current_id {
            continue;
        }
        let bytes = root
            .read_generation_bytes(id, limits.generation_bytes_max, &mut *is_cancelled)
            .map_err(|error| map_local_error(error, "referenced_generation"))?;
        let document =
            GenerationDocument::decode(id, project_id, &bytes, limits).map_err(|_| {
                ProjectStoreFault::Corruption {
                    stage: "referenced_generation",
                }
            })?;
        if expected_kind.is_some_and(|kind| document.kind() != kind)
            || document.forked_from() != authority.forked_from()
            || !document
                .projection()
                .state()
                .dataset()
                .has_same_scientific_content(authority.projection().state().dataset())
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "referenced_generation",
            });
        }
        validate_physical_closure_metadata(root, &document, limits, &mut *is_cancelled)?;
    }
    if manual_current.is_some_and(|manual_current| {
        autosave_current.is_some_and(|document| {
            document.forked_from() != manual_current.forked_from()
                || !document
                    .projection()
                    .state()
                    .dataset()
                    .has_same_scientific_content(manual_current.projection().state().dataset())
        })
    }) {
        return Err(ProjectStoreFault::Corruption {
            stage: "autosave_generation_continuity",
        });
    }
    Ok(())
}

fn insert_expected_generation(
    targets: &mut BTreeMap<ProjectGenerationId, Option<GenerationKind>>,
    id: ProjectGenerationId,
    kind: Option<GenerationKind>,
) -> Result<(), ProjectStoreFault> {
    match targets.entry(id) {
        Entry::Vacant(entry) => {
            entry.insert(kind);
            Ok(())
        }
        Entry::Occupied(mut entry) => match (*entry.get(), kind) {
            (Some(existing), Some(expected)) if existing != expected => {
                Err(ProjectStoreFault::Corruption {
                    stage: "generation_kind",
                })
            }
            (None, Some(expected)) => {
                entry.insert(Some(expected));
                Ok(())
            }
            _ => Ok(()),
        },
    }
}

fn validate_physical_closure_metadata<C>(
    root: &LocalStoreRoot,
    document: &GenerationDocument,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let logical = unique_logical_descriptors(document)?;
    let mut observed = BTreeMap::new();
    for (digest, storage) in document.bindings() {
        let descriptor = logical.get(digest).ok_or(ProjectStoreFault::Corruption {
            stage: "physical_closure",
        })?;
        match storage {
            ArtifactStorage::Direct { object } => {
                root.validate_exact_object_metadata(
                    object.digest(),
                    object.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut *is_cancelled,
                )
                .map_err(|error| map_local_error(error, "physical_object"))?;
                insert_closure(&mut observed, *object, limits)?;
            }
            ArtifactStorage::Paged { binding_manifest } => {
                let bytes = root
                    .read_exact_object_bytes(
                        binding_manifest.digest(),
                        binding_manifest.byte_length(),
                        limits.object_or_page_bytes_max,
                        &mut *is_cancelled,
                    )
                    .map_err(|error| map_local_error(error, "binding_manifest"))?;
                let binding =
                    LogicalObjectBinding::decode(&bytes, descriptor, binding_manifest, limits)
                        .map_err(|_| ProjectStoreFault::Corruption {
                            stage: "binding_manifest",
                        })?;
                for page in binding.pages() {
                    let page = page.object();
                    root.validate_exact_object_metadata(
                        page.digest(),
                        page.byte_length(),
                        limits.object_or_page_bytes_max,
                        &mut *is_cancelled,
                    )
                    .map_err(|error| map_local_error(error, "physical_object"))?;
                    insert_closure(&mut observed, page, limits)?;
                }
                insert_closure(
                    &mut observed,
                    PhysicalObject::new(binding_manifest.digest(), binding_manifest.byte_length()),
                    limits,
                )?;
            }
        }
    }
    let observed = observed.values().copied().collect::<Vec<_>>();
    if observed != document.reachable_objects() {
        return Err(ProjectStoreFault::Corruption {
            stage: "physical_closure",
        });
    }
    Ok(())
}

fn unique_logical_descriptors(
    document: &GenerationDocument,
) -> Result<BTreeMap<ExactBytesDigest, RawObjectDescriptor>, ProjectStoreFault> {
    let mut descriptors = BTreeMap::new();
    for artifact in document.projection().state().artifacts() {
        match descriptors.entry(artifact.object().digest()) {
            Entry::Vacant(entry) => {
                entry.insert(artifact.object().clone());
            }
            Entry::Occupied(entry) if entry.get() == artifact.object() => {}
            Entry::Occupied(_) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "logical_object",
                });
            }
        }
    }
    Ok(descriptors)
}

fn insert_closure(
    closure: &mut BTreeMap<ExactBytesDigest, PhysicalObject>,
    object: PhysicalObject,
    limits: ProjectStoreLimits,
) -> Result<(), ProjectStoreFault> {
    if let Some(existing) = closure.get(&object.digest()) {
        return if *existing == object {
            Ok(())
        } else {
            Err(ProjectStoreFault::Corruption {
                stage: "physical_closure",
            })
        };
    }
    if closure.len() >= limits.reachable_objects_per_generation_max {
        return Err(ProjectStoreFault::Capacity {
            stage: "physical_closure",
        });
    }
    closure.insert(object.digest(), object);
    Ok(())
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), ProjectStoreFault> {
    if is_cancelled() {
        Err(ProjectStoreFault::Cancelled)
    } else {
        Ok(())
    }
}

fn map_lease_error(error: LeaseError) -> ProjectStoreFault {
    match error {
        LeaseError::Indeterminate => ProjectStoreFault::CommitIndeterminate,
        LeaseError::InvalidAnchor | LeaseError::Io { .. } => {
            ProjectStoreFault::Corruption { stage: "lease" }
        }
    }
}

fn map_local_error(error: LocalPublicationError, stage: &'static str) -> ProjectStoreFault {
    match error {
        LocalPublicationError::Cancelled => ProjectStoreFault::Cancelled,
        LocalPublicationError::Capacity { .. } => ProjectStoreFault::Capacity { stage },
        LocalPublicationError::SourceLength { .. } => ProjectStoreFault::SourceChanged,
        LocalPublicationError::SourceDigest => ProjectStoreFault::DigestMismatch,
        LocalPublicationError::RefAlreadyPresent => ProjectStoreFault::StaleParent,
        LocalPublicationError::RefChanged => ProjectStoreFault::Corruption { stage },
        LocalPublicationError::RefCommitIndeterminate
        | LocalPublicationError::PackageCommitIndeterminate => {
            ProjectStoreFault::CommitIndeterminate
        }
        LocalPublicationError::DestinationExists => ProjectStoreFault::DestinationExists,
        LocalPublicationError::AtomicPublishUnsupported => ProjectStoreFault::UnsupportedFilesystem,
        LocalPublicationError::InvalidPath
        | LocalPublicationError::ExistingMismatch
        | LocalPublicationError::InvalidGeneration
        | LocalPublicationError::InvalidControl
        | LocalPublicationError::Io { .. } => ProjectStoreFault::Corruption { stage },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    const RECOVERABLE_MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
    );
    const RECOVERABLE_MANUAL_PREVIOUS: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "9cf3985edc9a7de3702029a4b32fd3e4188796ee8459deddd0c6cd7babf57d81"
    );
    const RECOVERABLE_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d9504b896fd6a3fb21e52d227fcd284df654d4f063ea8ee0ca49fce0155e9b73"
    );
    const RECOVERABLE_AUTOSAVE_PREVIOUS: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "dc1669b5773f1708b72114fb171e69c92d551e946de567ddd30d0a7c9a19d63c"
    );
    const RECOVERABLE_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "cfd67414728bb345edb7d5eabffac2530f04ed3b768d720782efe88e2d7ca370"
    );
    const DIVERGENT_MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "6b91b33dbaa378598269005b027db7a0643e14babe4b7522a5a415a461f6a497"
    );
    const DIVERGENT_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "b9af2901b12b248533e53d2683fcf4db7d4b2eb33ef292413b8b5dc2cb8b951e"
    );
    const DIVERGENT_MANUAL_PREVIOUS: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "10447a78680ee73dcc5572d71d81f1ad99079fb1374979a8a7937453a149ae1c"
    );
    const DIVERGENT_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "10011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10"
    );
    const STALE_MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d5020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec"
    );
    const STALE_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "c357ffd5f7c051bf22877ffcd6680bdcd0f7db4068af93587e4a1f5bed0542a0"
    );
    const PROVISIONAL_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "a1a84e1b98686c1d9eda416177988e691695baed74244ff5b99136e839ab0cea"
    );
    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestProject(PathBuf);

    impl TestProject {
        fn extracted(label: &str, store: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-inspection-{label}-{}-{nonce}-{}.m4dproj",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let output = Command::new("tar")
                .arg("-xzf")
                .arg(fixture_archive())
                .arg("-C")
                .arg(&path)
                .arg("--strip-components=1")
                .arg(store)
                .output()
                .unwrap();
            assert!(output.status.success(), "failed to extract {store}");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn store_path(&self) -> ProjectStorePath {
            ProjectStorePath::new(self.0.clone()).unwrap()
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn established_fixtures_report_exact_heads_and_autosave_classification() {
        for (store, manual, autosave, classification) in [
            (
                "recoverable.m4dproj",
                RECOVERABLE_MANUAL,
                RECOVERABLE_AUTOSAVE,
                EstablishedAutosaveClassification::Newer,
            ),
            (
                "divergent.m4dproj",
                DIVERGENT_MANUAL,
                DIVERGENT_AUTOSAVE,
                EstablishedAutosaveClassification::Divergent,
            ),
            (
                "stale.m4dproj",
                STALE_MANUAL,
                STALE_AUTOSAVE,
                EstablishedAutosaveClassification::Stale,
            ),
        ] {
            let project = TestProject::extracted("fixture", store);
            let opened = open_established_store(
                &project.store_path(),
                ProjectOpenMode::ReadOnly,
                ProjectStoreLimits::default(),
                || false,
            )
            .unwrap();
            assert_eq!(opened.effective_mode(), ProjectOpenMode::ReadOnly);
            let inspection = opened.inspection();
            assert_eq!(inspection.manual().head.current(), generation_id(manual));
            assert_eq!(
                inspection.autosave().unwrap().head.current(),
                generation_id(autosave)
            );
            assert_eq!(inspection.autosave_classification(), Some(classification));
            assert_eq!(
                inspection
                    .manual_generation()
                    .projection()
                    .state()
                    .project_id(),
                inspection.project_id()
            );
            assert_eq!(
                inspection
                    .autosave_generation()
                    .unwrap()
                    .projection()
                    .state()
                    .project_id(),
                inspection.project_id()
            );
        }
    }

    #[test]
    fn provisional_fixture_has_one_healthy_autosave_authority_without_a_manual_head() {
        let project = TestProject::extracted("provisional", "provisional.m4dproj");
        let before = all_file_bytes(project.path());
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let inspection =
            inspect_store_state(&root, ProjectStoreLimits::default(), || false).unwrap();

        assert!(inspection.is_provisional());
        assert_eq!(inspection.manual(), None);
        assert!(inspection.manual_generation().is_none());
        let autosave = inspection.autosave().unwrap();
        assert_eq!(autosave.head.current(), generation_id(PROVISIONAL_AUTOSAVE));
        assert_eq!(autosave.head.previous(), None);
        assert_eq!(autosave.head.base(), None);
        assert_eq!(autosave.recovery, None);
        assert_eq!(
            inspection
                .autosave_generation()
                .unwrap()
                .base_manual_generation_id(),
            None
        );
        assert_eq!(
            inspection
                .authority_generation()
                .projection()
                .state()
                .project_id(),
            inspection.project_id()
        );
        assert_eq!(
            inspection.root_generation_ids(),
            BTreeSet::from([generation_id(PROVISIONAL_AUTOSAVE)])
        );
        assert!(matches!(
            inspect_established_store(&root, ProjectStoreLimits::default(), || false),
            Err(ProjectStoreFault::Corruption {
                stage: "manual_head"
            })
        ));
        assert_eq!(all_file_bytes(project.path()), before);
    }

    #[test]
    fn store_graph_reports_exact_fixture_roots_orphans_and_object_partition() {
        for (store, roots, orphans, provisional) in [
            (
                "recoverable.m4dproj",
                vec![
                    RECOVERABLE_MANUAL,
                    RECOVERABLE_MANUAL_PREVIOUS,
                    RECOVERABLE_AUTOSAVE,
                    RECOVERABLE_AUTOSAVE_PREVIOUS,
                ],
                vec![RECOVERABLE_ORPHAN],
                false,
            ),
            (
                "divergent.m4dproj",
                vec![
                    DIVERGENT_MANUAL,
                    DIVERGENT_MANUAL_PREVIOUS,
                    DIVERGENT_AUTOSAVE,
                ],
                vec![DIVERGENT_ORPHAN],
                false,
            ),
            (
                "stale.m4dproj",
                vec![STALE_MANUAL, STALE_AUTOSAVE],
                vec![],
                false,
            ),
            (
                "provisional.m4dproj",
                vec![PROVISIONAL_AUTOSAVE],
                vec![],
                true,
            ),
        ] {
            let project = TestProject::extracted("graph", store);
            let before = all_file_bytes(project.path());
            let root = LocalStoreRoot::open(project.path()).unwrap();
            let graph =
                inspect_store_graph(&root, ProjectStoreLimits::default(), || false).unwrap();
            let mut expected_roots = roots.into_iter().map(generation_id).collect::<Vec<_>>();
            expected_roots.sort_unstable();
            let mut expected_orphans = orphans.into_iter().map(generation_id).collect::<Vec<_>>();
            expected_orphans.sort_unstable();

            assert_eq!(graph.state().is_provisional(), provisional);
            assert_eq!(graph.root_generation_ids(), expected_roots);
            assert_eq!(graph.orphan_generation_ids(), expected_orphans);
            assert_eq!(
                graph.generation_ids().len(),
                expected_roots.len() + expected_orphans.len()
            );
            assert_eq!(
                graph.object_facts().len(),
                graph.live_object_facts().len() + graph.unrooted_object_facts().len()
            );
            let live = graph
                .live_object_facts()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            assert!(
                graph
                    .unrooted_object_facts()
                    .iter()
                    .all(|object| !live.contains(object))
            );
            assert_eq!(all_file_bytes(project.path()), before);
        }
    }

    #[test]
    fn store_graph_rejects_corrupt_or_excess_orphans_without_affecting_normal_open() {
        let corrupt = TestProject::extracted("graph-corrupt", "recoverable.m4dproj");
        let root = LocalStoreRoot::open(corrupt.path()).unwrap();
        let orphan = generation_path(corrupt.path(), generation_id(RECOVERABLE_ORPHAN));
        let mut bytes = fs::read(&orphan).unwrap();
        bytes[0] ^= 1;
        fs::write(&orphan, bytes).unwrap();
        let before = all_file_bytes(corrupt.path());
        assert!(inspect_established_store(&root, ProjectStoreLimits::default(), || false).is_ok());
        assert!(matches!(
            inspect_store_graph(&root, ProjectStoreLimits::default(), || false),
            Err(ProjectStoreFault::Corruption {
                stage: "generation_graph"
            })
        ));
        assert_eq!(all_file_bytes(corrupt.path()), before);

        let capacity = TestProject::extracted("graph-capacity", "recoverable.m4dproj");
        let root = LocalStoreRoot::open(capacity.path()).unwrap();
        let before = all_file_bytes(capacity.path());
        let limits = ProjectStoreLimits {
            generations_scanned_max: 4,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            inspect_store_graph(&root, limits, || false),
            Err(ProjectStoreFault::Capacity {
                stage: "generation_namespace"
            })
        ));
        assert_eq!(all_file_bytes(capacity.path()), before);

        let candidates = TestProject::extracted("graph-candidates", "recoverable.m4dproj");
        let project_id = crate::wire::ProjectEnvelope::decode(
            &fs::read(candidates.path().join("project.json")).unwrap(),
        )
        .unwrap()
        .project_id();
        fs::write(
            candidates.path().join("refs/autosave-head"),
            RefRecord::new(
                RefKind::AutosaveHead,
                project_id,
                generation_id(RECOVERABLE_AUTOSAVE_PREVIOUS),
                None,
                Some(generation_id(RECOVERABLE_MANUAL)),
            )
            .unwrap()
            .encode(),
        )
        .unwrap();
        fs::remove_file(candidates.path().join("refs/autosave-recovery")).unwrap();
        let before = all_file_bytes(candidates.path());
        let root = LocalStoreRoot::open(candidates.path()).unwrap();
        let limits = ProjectStoreLimits {
            recovery_candidates_max: 1,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            inspect_store_graph(&root, limits, || false),
            Err(ProjectStoreFault::Capacity {
                stage: "recovery_candidates"
            })
        ));
        assert_eq!(all_file_bytes(candidates.path()), before);

        let provenance = TestProject::extracted("graph-provenance", "recoverable.m4dproj");
        let project_id = crate::wire::ProjectEnvelope::decode(
            &fs::read(provenance.path().join("project.json")).unwrap(),
        )
        .unwrap()
        .project_id();
        let old_id = generation_id(RECOVERABLE_ORPHAN);
        let old_bytes = fs::read(generation_path(provenance.path(), old_id)).unwrap();
        let old = GenerationDocument::decode(
            old_id,
            project_id,
            &old_bytes,
            ProjectStoreLimits::default(),
        )
        .unwrap();
        let replacement = GenerationDocument::build_from_projection(
            old.projection().clone(),
            Some(generation_id(RECOVERABLE_AUTOSAVE)),
            old.base_manual_generation_id(),
            old.forked_from(),
            old.kind(),
            old.generation_sequence(),
            old.bindings().clone(),
            old.reachable_objects().to_vec(),
            ProjectStoreLimits::default(),
        )
        .unwrap()
        .encode(ProjectStoreLimits::default())
        .unwrap();
        fs::remove_file(generation_path(provenance.path(), old_id)).unwrap();
        let replacement_path = generation_path(provenance.path(), replacement.id());
        fs::create_dir_all(replacement_path.parent().unwrap()).unwrap();
        fs::write(replacement_path, replacement.bytes()).unwrap();
        let before = all_file_bytes(provenance.path());
        let root = LocalStoreRoot::open(provenance.path()).unwrap();
        assert!(matches!(
            inspect_store_graph(&root, ProjectStoreLimits::default(), || false),
            Err(ProjectStoreFault::Corruption {
                stage: "generation_provenance"
            })
        ));
        assert_eq!(all_file_bytes(provenance.path()), before);
    }

    #[test]
    fn requested_mode_and_writer_contention_are_reported_without_losing_leases() {
        let project = TestProject::extracted("mode", "stale.m4dproj");
        let read_only = open_established_store(
            &project.store_path(),
            ProjectOpenMode::ReadOnly,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(read_only.effective_mode(), ProjectOpenMode::ReadOnly);
        drop(read_only);

        let writer = open_established_store(
            &project.store_path(),
            ProjectOpenMode::PreferWritable,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(writer.effective_mode(), ProjectOpenMode::PreferWritable);
        let fallback = open_established_store(
            &project.store_path(),
            ProjectOpenMode::PreferWritable,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(fallback.effective_mode(), ProjectOpenMode::ReadOnly);
        drop(fallback);
        drop(writer);

        let next = open_established_store(
            &project.store_path(),
            ProjectOpenMode::PreferWritable,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(next.effective_mode(), ProjectOpenMode::PreferWritable);
    }

    #[test]
    fn recovery_ahead_is_accepted_without_changing_head_authority_or_bytes() {
        let project = TestProject::extracted("recovery-ahead", "stale.m4dproj");
        let project_id = crate::wire::ProjectEnvelope::decode(
            &fs::read(project.path().join("project.json")).unwrap(),
        )
        .unwrap()
        .project_id();
        let manual = generation_id(STALE_MANUAL);
        let autosave = generation_id(STALE_AUTOSAVE);
        fs::write(
            project.path().join("refs/recovery"),
            RefRecord::new(RefKind::ManualRecovery, project_id, manual, None, None)
                .unwrap()
                .encode(),
        )
        .unwrap();
        fs::write(
            project.path().join("refs/autosave-recovery"),
            RefRecord::new(RefKind::AutosaveRecovery, project_id, autosave, None, None)
                .unwrap()
                .encode(),
        )
        .unwrap();
        let before = all_file_bytes(project.path());
        let opened = open_established_store(
            &project.store_path(),
            ProjectOpenMode::ReadOnly,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(opened.inspection().manual().head.current(), manual);
        assert_eq!(
            opened.inspection().autosave().unwrap().head.current(),
            autosave
        );
        drop(opened);
        assert_eq!(all_file_bytes(project.path()), before);
    }

    #[test]
    fn eager_inspection_checks_object_metadata_but_defers_payload_digests() {
        let changed = TestProject::extracted("changed-bytes", "stale.m4dproj");
        let object = stale_object_path(changed.path());
        let original = fs::read(&object).unwrap();
        fs::write(&object, vec![0x5a; original.len()]).unwrap();
        let opened = open_established_store(
            &changed.store_path(),
            ProjectOpenMode::ReadOnly,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let physical = opened.inspection().manual_generation().reachable_objects()[0];
        drop(opened);
        let root = LocalStoreRoot::open(changed.path()).unwrap();
        assert!(
            root.read_exact_object(
                physical.digest(),
                physical.byte_length(),
                ProjectStoreLimits::default().object_or_page_bytes_max,
                || false,
                |_| {}
            )
            .is_err()
        );

        let hardlinked = TestProject::extracted("hardlink", "stale.m4dproj");
        fs::hard_link(
            stale_object_path(hardlinked.path()),
            hardlinked.path().join("extra-link"),
        )
        .unwrap();
        assert_corrupt_open(&hardlinked);

        let linked = TestProject::extracted("symlink", "stale.m4dproj");
        let object = stale_object_path(linked.path());
        let outside = linked.path().with_extension("outside");
        fs::write(&outside, fs::read(&object).unwrap()).unwrap();
        fs::remove_file(&object).unwrap();
        symlink(&outside, &object).unwrap();
        assert_corrupt_open(&linked);
        let _ = fs::remove_file(outside);

        let truncated = TestProject::extracted("truncated", "stale.m4dproj");
        let object = stale_object_path(truncated.path());
        let mut bytes = fs::read(&object).unwrap();
        bytes.pop();
        fs::write(object, bytes).unwrap();
        assert_corrupt_open(&truncated);
    }

    #[test]
    fn cancellation_capacity_and_control_corruption_fail_without_mutation() {
        let cancelled = TestProject::extracted("cancelled", "stale.m4dproj");
        let before = all_file_bytes(cancelled.path());
        assert!(matches!(
            open_established_store(
                &cancelled.store_path(),
                ProjectOpenMode::ReadOnly,
                ProjectStoreLimits::default(),
                || true
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert_eq!(all_file_bytes(cancelled.path()), before);

        let capacity = TestProject::extracted("capacity", "stale.m4dproj");
        let before = all_file_bytes(capacity.path());
        let limits = ProjectStoreLimits {
            physical_store_entries_max: 1,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            open_established_store(
                &capacity.store_path(),
                ProjectOpenMode::ReadOnly,
                limits,
                || false
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "store_inventory"
            })
        ));
        assert_eq!(all_file_bytes(capacity.path()), before);

        let corrupt = TestProject::extracted("control", "stale.m4dproj");
        let mut envelope = fs::read(corrupt.path().join("project.json")).unwrap();
        envelope.push(b'\n');
        fs::write(corrupt.path().join("project.json"), envelope).unwrap();
        let before = all_file_bytes(corrupt.path());
        assert_corrupt_open(&corrupt);
        assert_eq!(all_file_bytes(corrupt.path()), before);
    }

    fn assert_corrupt_open(project: &TestProject) {
        assert!(matches!(
            open_established_store(
                &project.store_path(),
                ProjectOpenMode::ReadOnly,
                ProjectStoreLimits::default(),
                || false
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
    }

    fn stale_object_path(root: &Path) -> PathBuf {
        root.join(
            "objects/sha256/f3/17b2208b90efc088e10edda67cef73f8cedda059cb53538183fa94e12df94d",
        )
    }

    fn generation_path(root: &Path, id: ProjectGenerationId) -> PathBuf {
        let digest = id.digest().to_string();
        root.join("generations")
            .join("sha256")
            .join(&digest[..2])
            .join(format!("{}.json", &digest[2..]))
    }

    fn generation_id(value: &str) -> ProjectGenerationId {
        ProjectGenerationId::parse(value).unwrap()
    }

    fn all_file_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    fn fixture_archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/project/project-store-v1.tar.gz")
    }
}
