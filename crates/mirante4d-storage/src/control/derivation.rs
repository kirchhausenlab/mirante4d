use mirante4d_identity::{DerivationRecordId, ExactBytesDigest, RecipeId};
use serde::{Deserialize, Serialize};

use super::{
    AsciiToken, CONTROL_COLLECTION_ITEMS_MAX, CanonicalMapEntry, CanonicalValue, ControlError,
    MAX_PORTABLE_CONTROL_OBJECT_BYTES, TypedId, U64Decimal, jcs, value::WireValue,
};

const OBJECT: &str = "derivation payload";
const MAX_SPACE_BOXES: usize = 256;

/// One role-labelled typed input or output binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationBinding {
    role: AsciiToken,
    id: TypedId,
}

impl DerivationBinding {
    pub const fn new(role: AsciiToken, id: TypedId) -> Self {
        Self { role, id }
    }

    pub const fn role(&self) -> &AsciiToken {
        &self.role
    }

    pub const fn id(&self) -> TypedId {
        self.id
    }
}

/// One inclusive selected time range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DerivationTimeRange {
    start: U64Decimal,
    end: U64Decimal,
}

impl DerivationTimeRange {
    pub fn new(start: U64Decimal, end: U64Decimal) -> Result<Self, ControlError> {
        if start > end {
            return invalid("derivation time range start must not exceed end");
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> U64Decimal {
        self.start
    }

    pub const fn end(self) -> U64Decimal {
        self.end
    }
}

/// One half-open base-grid `t,z,y,x` scope box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DerivationSpaceBox {
    origin_tzyx: [U64Decimal; 4],
    extent_tzyx: [U64Decimal; 4],
}

impl DerivationSpaceBox {
    pub fn new(
        origin_tzyx: [U64Decimal; 4],
        extent_tzyx: [U64Decimal; 4],
    ) -> Result<Self, ControlError> {
        if extent_tzyx.iter().any(|extent| extent.get() == 0) {
            return invalid("derivation space-box extents must be positive");
        }
        for axis in 0..4 {
            origin_tzyx[axis]
                .get()
                .checked_add(extent_tzyx[axis].get())
                .ok_or(ControlError::InvalidControlObject {
                    object: OBJECT,
                    reason: "derivation space-box exclusive end overflowed u64",
                })?;
        }
        Ok(Self {
            origin_tzyx,
            extent_tzyx,
        })
    }

    pub const fn origin_tzyx(self) -> [U64Decimal; 4] {
        self.origin_tzyx
    }

    pub const fn extent_tzyx(self) -> [U64Decimal; 4] {
        self.extent_tzyx
    }

    fn overlaps(self, other: Self) -> bool {
        (0..4).all(|axis| {
            let self_end = self.origin_tzyx[axis].get() + self.extent_tzyx[axis].get();
            let other_end = other.origin_tzyx[axis].get() + other.extent_tzyx[axis].get();
            self.origin_tzyx[axis].get() < other_end && other.origin_tzyx[axis].get() < self_end
        })
    }
}

/// The closed selected layer/time/space scope of a derivation execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationScope {
    layers: Vec<U64Decimal>,
    time: Vec<DerivationTimeRange>,
    space: Vec<DerivationSpaceBox>,
}

impl DerivationScope {
    pub fn new(
        layers: Vec<U64Decimal>,
        time: Vec<DerivationTimeRange>,
        space: Vec<DerivationSpaceBox>,
    ) -> Result<Self, ControlError> {
        if layers.is_empty()
            || layers.len() > CONTROL_COLLECTION_ITEMS_MAX
            || layers.iter().any(|layer| layer.get() > u64::from(u32::MAX))
            || !layers.windows(2).all(|pair| pair[0] < pair[1])
        {
            return invalid(
                "derivation scope layers must be nonempty, sorted, unique u32 ordinals",
            );
        }
        if time.is_empty()
            || time.len() > CONTROL_COLLECTION_ITEMS_MAX
            || !time
                .windows(2)
                .all(|pair| pair[0] < pair[1] && pair[0].end.get() < pair[1].start.get())
        {
            return invalid("derivation time ranges must be nonempty, sorted, and nonoverlapping");
        }
        if space.is_empty()
            || space.len() > MAX_SPACE_BOXES
            || !space.windows(2).all(|pair| pair[0] < pair[1])
        {
            return invalid("derivation space boxes must be one through 256 sorted unique boxes");
        }
        for left in 0..space.len() {
            if space[left + 1..]
                .iter()
                .any(|right| space[left].overlaps(*right))
            {
                return invalid("derivation space boxes must be pairwise nonoverlapping");
            }
        }
        Ok(Self {
            layers,
            time,
            space,
        })
    }

    pub fn layers(&self) -> &[U64Decimal] {
        &self.layers
    }

    pub fn time(&self) -> &[DerivationTimeRange] {
        &self.time
    }

    pub fn space(&self) -> &[DerivationSpaceBox] {
        &self.space
    }

    pub fn canonical_value(&self) -> Result<CanonicalValue, ControlError> {
        let layers = CanonicalValue::list(
            self.layers
                .iter()
                .copied()
                .map(CanonicalValue::from_u64)
                .collect(),
        )?;
        let time = CanonicalValue::list(
            self.time
                .iter()
                .map(|range| {
                    CanonicalValue::map(vec![
                        entry("end", CanonicalValue::from_u64(range.end)),
                        entry("start", CanonicalValue::from_u64(range.start)),
                    ])
                })
                .collect::<Result<Vec<_>, _>>()?,
        )?;
        let space = CanonicalValue::list(
            self.space
                .iter()
                .map(|scope_box| {
                    CanonicalValue::map(vec![
                        entry(
                            "extent_tzyx",
                            CanonicalValue::list(
                                scope_box
                                    .extent_tzyx
                                    .iter()
                                    .copied()
                                    .map(CanonicalValue::from_u64)
                                    .collect(),
                            )?,
                        ),
                        entry(
                            "origin_tzyx",
                            CanonicalValue::list(
                                scope_box
                                    .origin_tzyx
                                    .iter()
                                    .copied()
                                    .map(CanonicalValue::from_u64)
                                    .collect(),
                            )?,
                        ),
                    ])
                })
                .collect::<Result<Vec<_>, _>>()?,
        )?;
        CanonicalValue::map(vec![
            entry("layers", layers),
            entry("space", space),
            entry("time", time),
        ])
    }

    fn from_canonical_value(value: CanonicalValue) -> Result<Self, ControlError> {
        let outer = exact_map(&value, &["layers", "space", "time"])?;
        let layers = exact_list(outer[0].value())?
            .iter()
            .map(expect_u64)
            .collect::<Result<Vec<_>, _>>()?;
        let time = exact_list(outer[2].value())?
            .iter()
            .map(|value| {
                let entries = exact_map(value, &["end", "start"])?;
                DerivationTimeRange::new(
                    expect_u64(entries[1].value())?,
                    expect_u64(entries[0].value())?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let space = exact_list(outer[1].value())?
            .iter()
            .map(|value| {
                let entries = exact_map(value, &["extent_tzyx", "origin_tzyx"])?;
                DerivationSpaceBox::new(
                    expect_u64_array(entries[1].value())?,
                    expect_u64_array(entries[0].value())?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(layers, time, space)
    }
}

/// Immutable implementation provenance for one execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationImplementation {
    name: AsciiToken,
    version: AsciiToken,
    build: ExactBytesDigest,
}

impl DerivationImplementation {
    pub const fn new(name: AsciiToken, version: AsciiToken, build: ExactBytesDigest) -> Self {
        Self {
            name,
            version,
            build,
        }
    }

    pub const fn name(&self) -> &AsciiToken {
        &self.name
    }

    pub const fn version(&self) -> &AsciiToken {
        &self.version
    }

    pub const fn build(&self) -> ExactBytesDigest {
        self.build
    }
}

/// The closed terminal execution outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivationOutcome {
    Success,
    Failed,
    Cancelled,
}

impl DerivationOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The closed exactness statement for an execution record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivationExactness {
    Exact,
    Approximate,
}

impl DerivationExactness {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Approximate => "approximate",
        }
    }
}

/// The identity-bearing canonical body of one recipe execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationBody {
    recipe_id: RecipeId,
    inputs: Vec<DerivationBinding>,
    outputs: Vec<DerivationBinding>,
    scope: DerivationScope,
    implementation: DerivationImplementation,
    outcome: DerivationOutcome,
    exactness: DerivationExactness,
}

impl DerivationBody {
    pub fn new(
        recipe_id: RecipeId,
        inputs: Vec<DerivationBinding>,
        outputs: Vec<DerivationBinding>,
        scope: DerivationScope,
        implementation: DerivationImplementation,
        outcome: DerivationOutcome,
        exactness: DerivationExactness,
    ) -> Result<Self, ControlError> {
        validate_bindings(&inputs, "inputs")?;
        validate_bindings(&outputs, "outputs")?;
        Ok(Self {
            recipe_id,
            inputs,
            outputs,
            scope,
            implementation,
            outcome,
            exactness,
        })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        validate_bindings(&self.inputs, "inputs")?;
        validate_bindings(&self.outputs, "outputs")?;
        let value = serde_json::to_value(WireDerivationBody::from(self)).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        jcs::encode(&value, "derivation body", MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub const fn recipe_id(&self) -> RecipeId {
        self.recipe_id
    }

    pub fn inputs(&self) -> &[DerivationBinding] {
        &self.inputs
    }

    pub fn outputs(&self) -> &[DerivationBinding] {
        &self.outputs
    }

    pub const fn scope(&self) -> &DerivationScope {
        &self.scope
    }

    pub const fn implementation(&self) -> &DerivationImplementation {
        &self.implementation
    }

    pub const fn outcome(&self) -> DerivationOutcome {
        self.outcome
    }

    pub const fn exactness(&self) -> DerivationExactness {
        self.exactness
    }
}

/// A canonical derivation body bound to its verified version-1 identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationPayload {
    derivation_record_id: DerivationRecordId,
    body: DerivationBody,
}

impl DerivationPayload {
    pub fn new(body: DerivationBody) -> Result<Self, ControlError> {
        let body_bytes = body.canonical_bytes()?;
        let derivation_record_id = DerivationRecordId::from_canonical_body_bytes(&body_bytes)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "derivation body length exceeds identity framing",
            })?;
        Ok(Self {
            derivation_record_id,
            body,
        })
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireDerivationPayload = serde_json::from_slice(bytes).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        let value = Self::try_from_wire(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject { object: OBJECT });
        }
        Ok(value)
    }

    pub(super) fn try_from_wire(wire: WireDerivationPayload) -> Result<Self, ControlError> {
        let declared = DerivationRecordId::parse(&wire.derivation_record_id).map_err(|_| {
            ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "derivation_record_id is invalid",
            }
        })?;
        let payload = Self::new(DerivationBody::try_from(wire.body)?)?;
        if payload.derivation_record_id != declared {
            return invalid("derivation_record_id does not verify the canonical body");
        }
        Ok(payload)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        let body_bytes = self.body.canonical_bytes()?;
        if !self
            .derivation_record_id
            .matches_canonical_body_bytes(&body_bytes)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "derivation body length exceeds identity framing",
            })?
        {
            return invalid("derivation_record_id does not verify the canonical body");
        }
        let value = serde_json::to_value(WireDerivationPayload::from(self)).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        jcs::encode(&value, OBJECT, MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub const fn derivation_record_id(&self) -> DerivationRecordId {
        self.derivation_record_id
    }

    pub const fn body(&self) -> &DerivationBody {
        &self.body
    }
}

fn validate_bindings(
    bindings: &[DerivationBinding],
    label: &'static str,
) -> Result<(), ControlError> {
    if bindings.len() > CONTROL_COLLECTION_ITEMS_MAX {
        return invalid(match label {
            "inputs" => "derivation inputs exceed 4096 bindings",
            _ => "derivation outputs exceed 4096 bindings",
        });
    }
    for pair in bindings.windows(2) {
        let left = (pair[0].role.as_str(), pair[0].id.to_string());
        let right = (pair[1].role.as_str(), pair[1].id.to_string());
        if (left.0.as_bytes(), left.1.as_bytes()) >= (right.0.as_bytes(), right.1.as_bytes()) {
            return invalid(match label {
                "inputs" => "derivation inputs must be strictly sorted and unique by role then id",
                _ => "derivation outputs must be strictly sorted and unique by role then id",
            });
        }
    }
    Ok(())
}

fn token(value: &'static str) -> AsciiToken {
    AsciiToken::parse(value).expect("fixed derivation scope keys are valid ASCII tokens")
}

fn entry(key: &'static str, value: CanonicalValue) -> CanonicalMapEntry {
    CanonicalMapEntry::new(token(key), value)
}

fn exact_map<'a>(
    value: &'a CanonicalValue,
    keys: &[&str],
) -> Result<&'a [CanonicalMapEntry], ControlError> {
    let entries = value.as_map().ok_or(ControlError::InvalidControlObject {
        object: OBJECT,
        reason: "derivation scope entry must be a canonical map",
    })?;
    if entries.len() != keys.len()
        || !entries
            .iter()
            .zip(keys)
            .all(|(entry, key)| entry.key().as_str() == *key)
    {
        return invalid("derivation scope map has the wrong closed keys");
    }
    Ok(entries)
}

fn exact_list(value: &CanonicalValue) -> Result<&[CanonicalValue], ControlError> {
    value.as_list().ok_or(ControlError::InvalidControlObject {
        object: OBJECT,
        reason: "derivation scope entry must be a canonical list",
    })
}

fn expect_u64(value: &CanonicalValue) -> Result<U64Decimal, ControlError> {
    value.as_u64().ok_or(ControlError::InvalidControlObject {
        object: OBJECT,
        reason: "derivation scope scalar must be a canonical u64",
    })
}

fn expect_u64_array(value: &CanonicalValue) -> Result<[U64Decimal; 4], ControlError> {
    exact_list(value)?
        .iter()
        .map(expect_u64)
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "derivation scope tzyx arrays must contain exactly four values",
        })
}

fn invalid<T>(reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidControlObject {
        object: OBJECT,
        reason,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireDerivationPayload {
    derivation_record_id: String,
    body: WireDerivationBody,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDerivationBody {
    recipe_id: String,
    inputs: Vec<WireDerivationBinding>,
    outputs: Vec<WireDerivationBinding>,
    scope: WireValue,
    implementation: WireDerivationImplementation,
    outcome: String,
    exactness: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDerivationBinding {
    role: String,
    id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDerivationImplementation {
    name: String,
    version: String,
    build: String,
}

impl TryFrom<WireDerivationBody> for DerivationBody {
    type Error = ControlError;

    fn try_from(wire: WireDerivationBody) -> Result<Self, Self::Error> {
        let recipe_id =
            RecipeId::parse(&wire.recipe_id).map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "recipe_id is invalid",
            })?;
        let outcome = match wire.outcome.as_str() {
            "success" => DerivationOutcome::Success,
            "failed" => DerivationOutcome::Failed,
            "cancelled" => DerivationOutcome::Cancelled,
            _ => return invalid("derivation outcome is not admitted"),
        };
        let exactness = match wire.exactness.as_str() {
            "exact" => DerivationExactness::Exact,
            "approximate" => DerivationExactness::Approximate,
            _ => return invalid("derivation exactness is not admitted"),
        };
        Self::new(
            recipe_id,
            wire.inputs
                .into_iter()
                .map(DerivationBinding::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            wire.outputs
                .into_iter()
                .map(DerivationBinding::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            DerivationScope::from_canonical_value(CanonicalValue::try_from(wire.scope)?)?,
            DerivationImplementation::try_from(wire.implementation)?,
            outcome,
            exactness,
        )
    }
}

impl TryFrom<WireDerivationBinding> for DerivationBinding {
    type Error = ControlError;

    fn try_from(wire: WireDerivationBinding) -> Result<Self, Self::Error> {
        Ok(Self::new(
            AsciiToken::parse(&wire.role)?,
            TypedId::parse(&wire.id)?,
        ))
    }
}

impl TryFrom<WireDerivationImplementation> for DerivationImplementation {
    type Error = ControlError;

    fn try_from(wire: WireDerivationImplementation) -> Result<Self, Self::Error> {
        Ok(Self::new(
            AsciiToken::parse(&wire.name)?,
            AsciiToken::parse(&wire.version)?,
            ExactBytesDigest::parse(&wire.build).map_err(|_| {
                ControlError::InvalidControlObject {
                    object: OBJECT,
                    reason: "derivation implementation build digest is invalid",
                }
            })?,
        ))
    }
}

impl From<&DerivationPayload> for WireDerivationPayload {
    fn from(value: &DerivationPayload) -> Self {
        Self {
            derivation_record_id: value.derivation_record_id.to_string(),
            body: WireDerivationBody::from(&value.body),
        }
    }
}

impl From<&DerivationBody> for WireDerivationBody {
    fn from(value: &DerivationBody) -> Self {
        Self {
            recipe_id: value.recipe_id.to_string(),
            inputs: value
                .inputs
                .iter()
                .map(WireDerivationBinding::from)
                .collect(),
            outputs: value
                .outputs
                .iter()
                .map(WireDerivationBinding::from)
                .collect(),
            scope: WireValue::from(
                &value
                    .scope
                    .canonical_value()
                    .expect("validated derivation scope remains encodable"),
            ),
            implementation: WireDerivationImplementation::from(&value.implementation),
            outcome: value.outcome.as_str().to_owned(),
            exactness: value.exactness.as_str().to_owned(),
        }
    }
}

impl From<&DerivationBinding> for WireDerivationBinding {
    fn from(value: &DerivationBinding) -> Self {
        Self {
            role: value.role.to_string(),
            id: value.id.to_string(),
        }
    }
}

impl From<&DerivationImplementation> for WireDerivationImplementation {
    fn from(value: &DerivationImplementation) -> Self {
        Self {
            name: value.name.to_string(),
            version: value.version.to_string(),
            build: value.build.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(value: &str) -> U64Decimal {
        U64Decimal::parse(value).unwrap()
    }

    fn typed(prefix: &str, hex: char) -> TypedId {
        TypedId::parse(&format!("{prefix}{}", hex.to_string().repeat(64))).unwrap()
    }

    fn payload() -> DerivationPayload {
        let zero = number("0");
        let one = number("1");
        DerivationPayload::new(
            DerivationBody::new(
                RecipeId::parse(&format!("{}{}", RecipeId::PREFIX, "0".repeat(64))).unwrap(),
                vec![DerivationBinding::new(
                    AsciiToken::parse("source").unwrap(),
                    typed("m4d-sc-v1-sha256:", '1'),
                )],
                vec![DerivationBinding::new(
                    AsciiToken::parse("result").unwrap(),
                    typed("m4d-artifact-v1-sha256:", '2'),
                )],
                DerivationScope::new(
                    vec![zero],
                    vec![DerivationTimeRange::new(zero, zero).unwrap()],
                    vec![DerivationSpaceBox::new([zero; 4], [one; 4]).unwrap()],
                )
                .unwrap(),
                DerivationImplementation::new(
                    AsciiToken::parse("mirante4d").unwrap(),
                    AsciiToken::parse("0.1.0").unwrap(),
                    ExactBytesDigest::parse(&format!("sha256:{}", "3".repeat(64))).unwrap(),
                ),
                DerivationOutcome::Success,
                DerivationExactness::Exact,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn derivation_payload_roundtrips_exact_scope_and_verified_identity() {
        let payload = payload();
        let bytes = payload.canonical_bytes().unwrap();
        assert_eq!(DerivationPayload::parse_canonical(&bytes).unwrap(), payload);
        assert!(String::from_utf8_lossy(&bytes).contains("\"extent_tzyx\""));
        assert!(
            payload
                .derivation_record_id()
                .matches_canonical_body_bytes(&payload.body().canonical_bytes().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn derivation_rejects_bad_order_overlap_identity_and_encoding() {
        let zero = number("0");
        let one = number("1");
        let two = number("2");
        let ten = number("10");
        let hundred = number("100");
        assert!(DerivationTimeRange::new(one, zero).is_err());
        assert!(DerivationSpaceBox::new([zero; 4], [zero; 4]).is_err());
        let first = DerivationSpaceBox::new([zero; 4], [ten, two, two, two]).unwrap();
        let middle = DerivationSpaceBox::new([one, hundred, zero, zero], [one; 4]).unwrap();
        let last = DerivationSpaceBox::new([two, one, zero, zero], [one; 4]).unwrap();
        assert!(
            DerivationScope::new(
                vec![zero],
                vec![DerivationTimeRange::new(zero, zero).unwrap()],
                vec![first, middle, last]
            )
            .is_err()
        );

        let valid = payload();
        let canonical = String::from_utf8(valid.canonical_bytes().unwrap()).unwrap();
        let wrong_id = canonical.replacen(
            &valid.derivation_record_id().to_string(),
            &format!("{}{}", DerivationRecordId::PREFIX, "f".repeat(64)),
            1,
        );
        for wire in [
            canonical.replacen("\"body\":", "\"body\":{},\"body\":", 1),
            canonical.replacen("\"recipe_id\":", "\"extra\":false,\"recipe_id\":", 1),
            canonical.replacen(
                "\"key\":\"layers\"",
                "\"key\":\"layers\",\"key\":\"layers\"",
                1,
            ),
            format!(" {canonical}"),
            wrong_id,
        ] {
            assert!(DerivationPayload::parse_canonical(wire.as_bytes()).is_err());
        }
        assert!(
            DerivationPayload::parse_canonical(&vec![b' '; MAX_PORTABLE_CONTROL_OBJECT_BYTES + 1])
                .is_err()
        );
    }
}
