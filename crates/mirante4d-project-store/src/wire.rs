#![cfg_attr(not(test), allow(dead_code))]

use mirante4d_identity::{Sha256Digest, Sha256Hasher};
use mirante4d_project_model::ProjectId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::ProjectGenerationId;

const ENVELOPE_PROFILE: &str = "mirante4d-project-store-v1";
const ENVELOPE_SCHEMA: &str = "mirante4d-project-store-envelope";
const ENVELOPE_SCHEMA_VERSION: u32 = 1;
const ENVELOPE_BYTES_MAX: usize = 16_384;
const GENERATION_BYTES_MAX: usize = 67_108_864;
const GENERATION_DOMAIN: &[u8] = b"M4D-PROJECT-GENERATION-V1\0";
const REF_CHECKSUM_DOMAIN: &[u8] = b"M4D-PROJECT-REF-V1\0";
const REF_MAGIC: &[u8; 8] = b"M4DREF1\0";
const REF_SCHEMA_VERSION: u16 = 1;
pub(crate) const REF_BYTES: usize = 160;
const REF_CHECKSUM_OFFSET: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectEnvelope {
    project_id: ProjectId,
}

impl ProjectEnvelope {
    pub(crate) const fn new(project_id: ProjectId) -> Self {
        Self { project_id }
    }

    pub(crate) const fn project_id(self) -> ProjectId {
        self.project_id
    }

    pub(crate) fn encode(self) -> Result<Vec<u8>, WireError> {
        encode_canonical_json(&EnvelopeWire {
            profile: ENVELOPE_PROFILE,
            project_id: self.project_id.to_string(),
            schema: ENVELOPE_SCHEMA,
            schema_version: ENVELOPE_SCHEMA_VERSION,
        })
    }

    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, WireError> {
        if encoded.len() > ENVELOPE_BYTES_MAX {
            return Err(WireError::EnvelopeBytesLimit);
        }
        validate_canonical_json(encoded)?;
        let envelope: EnvelopeOwned =
            serde_json::from_slice(encoded).map_err(|_| WireError::EnvelopeShape)?;
        if envelope.schema != ENVELOPE_SCHEMA || envelope.schema_version != ENVELOPE_SCHEMA_VERSION
        {
            return Err(WireError::EnvelopeSchema);
        }
        if envelope.profile != ENVELOPE_PROFILE {
            return Err(WireError::EnvelopeProfile);
        }
        let project_id =
            ProjectId::parse(&envelope.project_id).map_err(|_| WireError::EnvelopeProjectId)?;
        Ok(Self { project_id })
    }
}

#[derive(Serialize)]
struct EnvelopeWire<'a> {
    profile: &'a str,
    project_id: String,
    schema: &'a str,
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeOwned {
    profile: String,
    project_id: String,
    schema: String,
    schema_version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RefKind {
    ManualHead = 1,
    ManualRecovery = 2,
    AutosaveHead = 3,
    AutosaveRecovery = 4,
    Pin = 5,
}

impl RefKind {
    fn decode(value: u8) -> Result<Self, WireError> {
        match value {
            1 => Ok(Self::ManualHead),
            2 => Ok(Self::ManualRecovery),
            3 => Ok(Self::AutosaveHead),
            4 => Ok(Self::AutosaveRecovery),
            5 => Ok(Self::Pin),
            actual => Err(WireError::UnknownRefKind { actual }),
        }
    }

    const fn permits_previous(self) -> bool {
        matches!(self, Self::ManualHead | Self::AutosaveHead)
    }

    const fn permits_base(self) -> bool {
        matches!(self, Self::AutosaveHead)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RefRecord {
    kind: RefKind,
    project_id: ProjectId,
    current: ProjectGenerationId,
    previous: Option<ProjectGenerationId>,
    base: Option<ProjectGenerationId>,
}

impl RefRecord {
    pub(crate) fn new(
        kind: RefKind,
        project_id: ProjectId,
        current: ProjectGenerationId,
        previous: Option<ProjectGenerationId>,
        base: Option<ProjectGenerationId>,
    ) -> Result<Self, WireError> {
        validate_ref_shape(kind, previous, base)?;
        Ok(Self {
            kind,
            project_id,
            current,
            previous,
            base,
        })
    }

    pub(crate) const fn kind(self) -> RefKind {
        self.kind
    }

    pub(crate) const fn project_id(self) -> ProjectId {
        self.project_id
    }

    pub(crate) const fn current(self) -> ProjectGenerationId {
        self.current
    }

    pub(crate) const fn previous(self) -> Option<ProjectGenerationId> {
        self.previous
    }

    pub(crate) const fn base(self) -> Option<ProjectGenerationId> {
        self.base
    }

    pub(crate) fn encode(self) -> [u8; REF_BYTES] {
        let mut encoded = [0_u8; REF_BYTES];
        encoded[..8].copy_from_slice(REF_MAGIC);
        encoded[8..10].copy_from_slice(&REF_SCHEMA_VERSION.to_be_bytes());
        encoded[10] = self.kind as u8;
        encoded[11] = u8::from(self.previous.is_some()) | (u8::from(self.base.is_some()) << 1);
        encoded[12..16].copy_from_slice(&(REF_BYTES as u32).to_be_bytes());
        encoded[16..32].copy_from_slice(&self.project_id.bytes());
        copy_generation_digest(&mut encoded[32..64], self.current);
        if let Some(previous) = self.previous {
            copy_generation_digest(&mut encoded[64..96], previous);
        }
        if let Some(base) = self.base {
            copy_generation_digest(&mut encoded[96..128], base);
        }
        let checksum = ref_checksum(&encoded[..REF_CHECKSUM_OFFSET]);
        encoded[REF_CHECKSUM_OFFSET..].copy_from_slice(checksum.as_bytes());
        encoded
    }

    pub(crate) fn decode(expected_kind: RefKind, encoded: &[u8]) -> Result<Self, WireError> {
        if encoded.len() != REF_BYTES {
            return Err(WireError::RefLength {
                actual: encoded.len(),
            });
        }
        if &encoded[..8] != REF_MAGIC {
            return Err(WireError::RefMagic);
        }
        let schema_version = u16::from_be_bytes([encoded[8], encoded[9]]);
        if schema_version != REF_SCHEMA_VERSION {
            return Err(WireError::RefSchemaVersion {
                actual: schema_version,
            });
        }
        let kind = RefKind::decode(encoded[10])?;
        if kind != expected_kind {
            return Err(WireError::RefKindMismatch {
                expected: expected_kind,
                actual: kind,
            });
        }
        if u32::from_be_bytes(encoded[12..16].try_into().expect("fixed ref header"))
            != REF_BYTES as u32
        {
            return Err(WireError::RefDeclaredLength);
        }
        let expected_checksum = ref_checksum(&encoded[..REF_CHECKSUM_OFFSET]);
        if encoded[REF_CHECKSUM_OFFSET..] != expected_checksum.as_bytes()[..] {
            return Err(WireError::RefChecksum);
        }

        let presence = encoded[11];
        if presence & !0b11 != 0 {
            return Err(WireError::RefPresence);
        }
        let previous_present = presence & 0b01 != 0;
        let base_present = presence & 0b10 != 0;
        if (!previous_present && encoded[64..96] != [0_u8; 32])
            || (!base_present && encoded[96..128] != [0_u8; 32])
        {
            return Err(WireError::RefPresence);
        }
        let previous = previous_present.then(|| generation_from_slot(&encoded[64..96]));
        let base = base_present.then(|| generation_from_slot(&encoded[96..128]));
        validate_ref_shape(kind, previous, base)?;

        Ok(Self {
            kind,
            project_id: ProjectId::from_bytes(encoded[16..32].try_into().expect("fixed UUID slot")),
            current: generation_from_slot(&encoded[32..64]),
            previous,
            base,
        })
    }
}

/// Frames bytes which a typed generation decoder has already validated.
///
/// This helper deliberately establishes only the frozen identity framing; it
/// is not evidence that arbitrary canonical JSON is a valid generation.
pub(crate) fn framed_generation_id(
    canonical_generation: &[u8],
) -> Result<ProjectGenerationId, WireError> {
    validate_generation_length(canonical_generation.len())?;
    validate_canonical_json(canonical_generation)?;
    let byte_length = u64::try_from(canonical_generation.len())
        .map_err(|_| WireError::GenerationLengthOverflow)?;
    let mut hasher = Sha256Hasher::new();
    hasher.update(GENERATION_DOMAIN);
    hasher.update(byte_length.to_be_bytes());
    hasher.update(canonical_generation);
    Ok(ProjectGenerationId::from_digest(hasher.finalize()))
}

fn validate_generation_length(length: usize) -> Result<(), WireError> {
    if length > GENERATION_BYTES_MAX {
        Err(WireError::GenerationBytesLimit)
    } else {
        Ok(())
    }
}

pub(crate) fn encode_canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let value = serde_json::to_value(value).map_err(|_| WireError::JsonEncode)?;
    let mut encoded = Vec::new();
    write_canonical_value(&value, &mut encoded)?;
    Ok(encoded)
}

pub(crate) fn validate_canonical_json(encoded: &[u8]) -> Result<(), WireError> {
    let value: Value = serde_json::from_slice(encoded).map_err(|_| WireError::InvalidJson)?;
    let mut canonical = Vec::new();
    write_canonical_value(&value, &mut canonical)?;
    if canonical != encoded {
        return Err(WireError::NonCanonicalJson);
    }
    Ok(())
}

fn write_canonical_value(value: &Value, output: &mut Vec<u8>) -> Result<(), WireError> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Number(number) => {
            let integer = number.as_u64().ok_or(WireError::UnsupportedJsonNumber)?;
            let integer = u32::try_from(integer).map_err(|_| WireError::UnsupportedJsonNumber)?;
            output.extend_from_slice(integer.to_string().as_bytes());
        }
        Value::String(string) => {
            serde_json::to_writer(output, string).map_err(|_| WireError::JsonEncode)?
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_value(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key).map_err(|_| WireError::JsonEncode)?;
                output.push(b':');
                write_canonical_value(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn validate_ref_shape(
    kind: RefKind,
    previous: Option<ProjectGenerationId>,
    base: Option<ProjectGenerationId>,
) -> Result<(), WireError> {
    if (previous.is_some() && !kind.permits_previous()) || (base.is_some() && !kind.permits_base())
    {
        return Err(WireError::RefShape);
    }
    Ok(())
}

fn copy_generation_digest(output: &mut [u8], generation: ProjectGenerationId) {
    output.copy_from_slice(generation.digest().as_bytes());
}

fn generation_from_slot(slot: &[u8]) -> ProjectGenerationId {
    ProjectGenerationId::from_digest(Sha256Digest::from_bytes(
        slot.try_into().expect("fixed generation digest slot"),
    ))
}

fn ref_checksum(header: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256Hasher::new();
    hasher.update(REF_CHECKSUM_DOMAIN);
    hasher.update(header);
    hasher.finalize()
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum WireError {
    #[error("JSON encoding failed")]
    JsonEncode,
    #[error("invalid JSON")]
    InvalidJson,
    #[error("JSON bytes are not canonical")]
    NonCanonicalJson,
    #[error("JSON number is outside the restricted canonical profile")]
    UnsupportedJsonNumber,
    #[error("project envelope has unknown, missing, or mistyped fields")]
    EnvelopeShape,
    #[error("project envelope exceeds the 16384-byte limit")]
    EnvelopeBytesLimit,
    #[error("project envelope schema is unsupported")]
    EnvelopeSchema,
    #[error("project envelope profile is unsupported")]
    EnvelopeProfile,
    #[error("project envelope project ID is invalid")]
    EnvelopeProjectId,
    #[error("ref record has {actual} bytes instead of 160")]
    RefLength { actual: usize },
    #[error("ref record magic is invalid")]
    RefMagic,
    #[error("ref schema version {actual} is unsupported")]
    RefSchemaVersion { actual: u16 },
    #[error("ref kind code {actual} is unknown")]
    UnknownRefKind { actual: u8 },
    #[error("ref kind {actual:?} does not match expected path kind {expected:?}")]
    RefKindMismatch { expected: RefKind, actual: RefKind },
    #[error("ref record declared length is invalid")]
    RefDeclaredLength,
    #[error("ref record checksum is invalid")]
    RefChecksum,
    #[error("ref record presence bits or zero slots are invalid")]
    RefPresence,
    #[error("ref record slots are forbidden for this kind")]
    RefShape,
    #[error("generation byte length cannot be represented as u64")]
    GenerationLengthOverflow,
    #[error("generation exceeds the 67108864-byte limit")]
    GenerationBytesLimit,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "11111111-2222-4333-8444-555555555555";
    const MANUAL_HEAD_HEX: &str = concat!(
        "4d3444524546310000010101000000a0",
        "11111111222243338444555555555555",
        "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854",
        "9cf3985edc9a7de3702029a4b32fd3e4188796ee8459deddd0c6cd7babf57d81",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0cf68ad1ce16b493d7e27968eca5794c17a42ef237fd9f590dc3abb63044e407",
    );

    #[test]
    fn envelope_matches_the_frozen_canonical_vector() {
        let envelope = ProjectEnvelope::new(ProjectId::parse(PROJECT).unwrap());
        let encoded = envelope.encode().unwrap();
        assert_eq!(
            encoded,
            br#"{"profile":"mirante4d-project-store-v1","project_id":"11111111-2222-4333-8444-555555555555","schema":"mirante4d-project-store-envelope","schema_version":1}"#,
        );
        let decoded = ProjectEnvelope::decode(&encoded).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.project_id(), ProjectId::parse(PROJECT).unwrap());
    }

    #[test]
    fn envelope_rejects_noncanonical_unknown_and_invalid_identity_bytes() {
        let canonical = ProjectEnvelope::new(ProjectId::parse(PROJECT).unwrap())
            .encode()
            .unwrap();
        let mut whitespace = canonical.clone();
        whitespace.push(b'\n');
        assert_eq!(
            ProjectEnvelope::decode(&whitespace),
            Err(WireError::NonCanonicalJson)
        );
        let unknown = canonical
            .strip_suffix(b"}")
            .unwrap()
            .iter()
            .copied()
            .chain(br#","unknown":true}"#.iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(
            ProjectEnvelope::decode(&unknown),
            Err(WireError::EnvelopeShape)
        );
        let invalid_id = canonical
            .windows(PROJECT.len())
            .position(|window| window == PROJECT.as_bytes())
            .unwrap();
        let mut invalid = canonical;
        invalid[invalid_id] = b'G';
        assert_eq!(
            ProjectEnvelope::decode(&invalid),
            Err(WireError::EnvelopeProjectId)
        );
        assert_eq!(
            ProjectEnvelope::decode(&vec![b' '; ENVELOPE_BYTES_MAX + 1]),
            Err(WireError::EnvelopeBytesLimit)
        );
    }

    #[test]
    fn manual_head_matches_the_independent_fixture_vector() {
        let frozen = decode_hex::<REF_BYTES>(MANUAL_HEAD_HEX);
        let decoded = RefRecord::decode(RefKind::ManualHead, &frozen).unwrap();
        assert_eq!(decoded.project_id().to_string(), PROJECT);
        assert_eq!(
            decoded.current().digest().to_string(),
            "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
        );
        assert_eq!(
            decoded.previous().unwrap().digest().to_string(),
            "9cf3985edc9a7de3702029a4b32fd3e4188796ee8459deddd0c6cd7babf57d81"
        );
        assert_eq!(decoded.base(), None);
        assert_eq!(decoded.encode(), frozen);
    }

    #[test]
    fn all_ref_shapes_round_trip_and_forbidden_slots_are_rejected() {
        let project = ProjectId::parse(PROJECT).unwrap();
        let current = generation(1);
        let previous = generation(2);
        let base = generation(3);
        for record in [
            RefRecord::new(RefKind::ManualHead, project, current, Some(previous), None).unwrap(),
            RefRecord::new(RefKind::ManualRecovery, project, current, None, None).unwrap(),
            RefRecord::new(
                RefKind::AutosaveHead,
                project,
                current,
                Some(previous),
                Some(base),
            )
            .unwrap(),
            RefRecord::new(RefKind::AutosaveHead, project, current, None, None).unwrap(),
            RefRecord::new(RefKind::AutosaveRecovery, project, current, None, None).unwrap(),
            RefRecord::new(RefKind::Pin, project, current, None, None).unwrap(),
        ] {
            assert_eq!(
                RefRecord::decode(record.kind(), &record.encode()),
                Ok(record)
            );
        }
        assert_eq!(
            RefRecord::new(
                RefKind::ManualRecovery,
                project,
                current,
                Some(previous),
                None
            ),
            Err(WireError::RefShape)
        );
        assert_eq!(
            RefRecord::new(RefKind::ManualHead, project, current, None, Some(base)),
            Err(WireError::RefShape)
        );
    }

    #[test]
    fn ref_decoder_rejects_length_checksum_kind_and_presence_corruption() {
        let frozen = decode_hex::<REF_BYTES>(MANUAL_HEAD_HEX);
        assert_eq!(
            RefRecord::decode(RefKind::ManualHead, &frozen[..REF_BYTES - 1]),
            Err(WireError::RefLength {
                actual: REF_BYTES - 1
            })
        );
        let mut checksum = frozen;
        checksum[159] ^= 1;
        assert_eq!(
            RefRecord::decode(RefKind::ManualHead, &checksum),
            Err(WireError::RefChecksum)
        );
        assert!(matches!(
            RefRecord::decode(RefKind::Pin, &frozen),
            Err(WireError::RefKindMismatch { .. })
        ));
        let mut presence = frozen;
        presence[11] |= 0x80;
        reseal(&mut presence);
        assert_eq!(
            RefRecord::decode(RefKind::ManualHead, &presence),
            Err(WireError::RefPresence)
        );
        let mut nonzero_absent = frozen;
        nonzero_absent[96] = 1;
        reseal(&mut nonzero_absent);
        assert_eq!(
            RefRecord::decode(RefKind::ManualHead, &nonzero_absent),
            Err(WireError::RefPresence)
        );
    }

    #[test]
    fn generation_identity_matches_hand_vector_and_rejects_noncanonical_json() {
        let identity = framed_generation_id(b"{}").unwrap();
        assert_eq!(
            identity.digest().to_string(),
            "c9135c0d5e0b5a2599b7f16533a44842fa5f635c503cf926bc55d94b71da6a25"
        );
        assert_eq!(
            framed_generation_id(b"{ }").unwrap_err(),
            WireError::NonCanonicalJson
        );
        assert_eq!(
            framed_generation_id(br#"{"z":0,"a":1}"#).unwrap_err(),
            WireError::NonCanonicalJson
        );
        assert_eq!(
            framed_generation_id(br#"{"value":4294967296}"#).unwrap_err(),
            WireError::UnsupportedJsonNumber
        );
        assert_eq!(
            framed_generation_id(br#"{"value":1.0}"#).unwrap_err(),
            WireError::UnsupportedJsonNumber
        );
        assert_eq!(
            validate_generation_length(GENERATION_BYTES_MAX + 1),
            Err(WireError::GenerationBytesLimit)
        );
    }

    fn generation(byte: u8) -> ProjectGenerationId {
        ProjectGenerationId::from_digest(Sha256Digest::from_bytes([byte; 32]))
    }

    fn reseal(encoded: &mut [u8; REF_BYTES]) {
        let checksum = ref_checksum(&encoded[..REF_CHECKSUM_OFFSET]);
        encoded[REF_CHECKSUM_OFFSET..].copy_from_slice(checksum.as_bytes());
    }

    fn decode_hex<const N: usize>(value: &str) -> [u8; N] {
        assert_eq!(value.len(), N * 2);
        let mut decoded = [0_u8; N];
        for (output, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            *output = (nibble(pair[0]) << 4) | nibble(pair[1]);
        }
        decoded
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("invalid test hex"),
        }
    }
}
