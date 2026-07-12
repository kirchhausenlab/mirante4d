use std::{fmt, str::FromStr};

use mirante4d_identity::ReleaseId;
use serde::{Deserialize, Serialize};

use super::{
    CONTROL_COLLECTION_ITEMS_MAX, ControlError, MAX_PORTABLE_CONTROL_OBJECT_BYTES, NfcText,
    PORTABLE_RECORD_COUNT_MAX, TypedId, U64Decimal,
    derivation::{DerivationPayload, WireDerivationPayload},
    jcs,
    recipe::{RecipePayload, WireRecipePayload},
};

const OBJECT: &str = "portable record";
const SCHEMA: &str = "m4d-portable-record";

/// A canonical lowercase DOI admitted by the experimental portable-record profile.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Doi(String);

impl Doi {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Doi {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > 255 || !value.is_ascii() {
            return invalid_scalar("DOI", "expected at most 255 lowercase ASCII bytes");
        }
        let Some(value) = value.strip_prefix("10.") else {
            return invalid_scalar(
                "DOI",
                "expected the canonical 10.<registrant>/<suffix> form",
            );
        };
        let Some((registrant, suffix)) = value.split_once('/') else {
            return invalid_scalar(
                "DOI",
                "expected the canonical 10.<registrant>/<suffix> form",
            );
        };
        if !(4..=9).contains(&registrant.len())
            || !registrant.bytes().all(|byte| byte.is_ascii_digit())
            || suffix.is_empty()
            || !suffix.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._;()/:-".contains(&byte)
            })
        {
            return invalid_scalar("DOI", "the registrant or suffix grammar is invalid");
        }
        Ok(Self(format!("10.{registrant}/{suffix}")))
    }
}

impl fmt::Display for Doi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The closed portable source-identifier scheme registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceIdentifierScheme {
    Doi,
    Sha256,
}

impl SourceIdentifierScheme {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        match value {
            "doi" => Ok(Self::Doi),
            "sha256" => Ok(Self::Sha256),
            _ => invalid_scalar("source identifier scheme", "the scheme is not admitted"),
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Doi => "doi",
            Self::Sha256 => "sha256",
        }
    }
}

impl fmt::Display for SourceIdentifierScheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One portable path-free source identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentifier {
    scheme: SourceIdentifierScheme,
    value: NfcText,
}

impl SourceIdentifier {
    pub fn new(scheme: SourceIdentifierScheme, value: NfcText) -> Result<Self, ControlError> {
        match scheme {
            SourceIdentifierScheme::Doi => {
                Doi::parse(value.as_str())?;
            }
            SourceIdentifierScheme::Sha256 => {
                if value.as_str().len() != 64
                    || !value
                        .as_str()
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                {
                    return invalid_scalar(
                        "source SHA-256",
                        "expected exactly 64 lowercase hexadecimal digits",
                    );
                }
            }
        }
        Ok(Self { scheme, value })
    }

    pub const fn scheme(&self) -> SourceIdentifierScheme {
        self.scheme
    }

    pub const fn value(&self) -> &NfcText {
        &self.value
    }
}

/// Portable source lineage that contains no machine-local locator facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePayload {
    source_identifiers: Vec<SourceIdentifier>,
    parent_release_id: Option<ReleaseId>,
}

impl SourcePayload {
    pub fn new(
        source_identifiers: Vec<SourceIdentifier>,
        parent_release_id: Option<ReleaseId>,
    ) -> Result<Self, ControlError> {
        validate_source_identifiers(&source_identifiers)?;
        Ok(Self {
            source_identifiers,
            parent_release_id,
        })
    }

    pub fn source_identifiers(&self) -> &[SourceIdentifier] {
        &self.source_identifiers
    }

    pub const fn parent_release_id(&self) -> Option<ReleaseId> {
        self.parent_release_id
    }
}

/// The closed version-1 SPDX license registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpdxLicense {
    CcBy40,
    CcZero10,
    Mit,
}

impl SpdxLicense {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        match value {
            "CC-BY-4.0" => Ok(Self::CcBy40),
            "CC0-1.0" => Ok(Self::CcZero10),
            "MIT" => Ok(Self::Mit),
            _ => invalid_scalar("SPDX license", "the license literal is not admitted"),
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::CcBy40 => "CC-BY-4.0",
            Self::CcZero10 => "CC0-1.0",
            Self::Mit => "MIT",
        }
    }
}

impl fmt::Display for SpdxLicense {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A closed portable license and rights-holder declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RightsPayload {
    license_spdx: SpdxLicense,
    rights_holders: Vec<NfcText>,
}

impl RightsPayload {
    pub fn new(
        license_spdx: SpdxLicense,
        rights_holders: Vec<NfcText>,
    ) -> Result<Self, ControlError> {
        validate_nfc_set(&rights_holders, "rights holders")?;
        Ok(Self {
            license_spdx,
            rights_holders,
        })
    }

    pub const fn license_spdx(&self) -> SpdxLicense {
        self.license_spdx
    }

    pub fn rights_holders(&self) -> &[NfcText] {
        &self.rights_holders
    }
}

/// Portable citation metadata whose creator order remains semantic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CitationPayload {
    title: NfcText,
    creators: Vec<NfcText>,
    year: Option<U64Decimal>,
    doi: Option<Doi>,
}

impl CitationPayload {
    pub fn new(
        title: NfcText,
        creators: Vec<NfcText>,
        year: Option<U64Decimal>,
        doi: Option<Doi>,
    ) -> Result<Self, ControlError> {
        if title.as_str().is_empty() {
            return invalid("citation title must be nonempty");
        }
        if creators.is_empty()
            || creators.len() > CONTROL_COLLECTION_ITEMS_MAX
            || creators.iter().any(|creator| creator.as_str().is_empty())
        {
            return invalid("citation creators must be nonempty text values");
        }
        if year.is_some_and(|year| !(1_000..=9_999).contains(&year.get())) {
            return invalid("citation year must be null or in 1000 through 9999");
        }
        validate_text_budget(
            std::iter::once(title.as_str().len())
                .chain(creators.iter().map(|creator| creator.as_str().len()))
                .chain(doi.iter().map(|doi| doi.as_str().len())),
        )?;
        Ok(Self {
            title,
            creators,
            year,
            doi,
        })
    }

    pub const fn title(&self) -> &NfcText {
        &self.title
    }

    pub fn creators(&self) -> &[NfcText] {
        &self.creators
    }

    pub const fn year(&self) -> Option<U64Decimal> {
        self.year
    }

    pub const fn doi(&self) -> Option<&Doi> {
        self.doi.as_ref()
    }
}

/// The closed portable-record kind registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortableRecordKind {
    Source,
    Recipe,
    Derivation,
    Rights,
    Citation,
}

impl PortableRecordKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Recipe => "recipe",
            Self::Derivation => "derivation",
            Self::Rights => "rights",
            Self::Citation => "citation",
        }
    }
}

/// The exact kind-selected payload of a portable record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortableRecordPayload {
    Source(SourcePayload),
    Recipe(RecipePayload),
    Derivation(DerivationPayload),
    Rights(RightsPayload),
    Citation(CitationPayload),
}

impl PortableRecordPayload {
    pub const fn kind(&self) -> PortableRecordKind {
        match self {
            Self::Source(_) => PortableRecordKind::Source,
            Self::Recipe(_) => PortableRecordKind::Recipe,
            Self::Derivation(_) => PortableRecordKind::Derivation,
            Self::Rights(_) => PortableRecordKind::Rights,
            Self::Citation(_) => PortableRecordKind::Citation,
        }
    }

    fn validate(&self) -> Result<(), ControlError> {
        match self {
            Self::Source(payload) => validate_source_identifiers(&payload.source_identifiers),
            Self::Recipe(payload) => payload.canonical_bytes().map(|_| ()),
            Self::Derivation(payload) => {
                payload.canonical_bytes()?;
                if payload.body().outputs().iter().any(|binding| {
                    matches!(binding.id(), TypedId::Package(_) | TypedId::Release(_))
                }) {
                    return invalid(
                        "package-contained derivation outputs cannot name PackageId or ReleaseId",
                    );
                }
                Ok(())
            }
            Self::Rights(payload) => validate_nfc_set(&payload.rights_holders, "rights holders"),
            Self::Citation(payload) => CitationPayload::new(
                payload.title.clone(),
                payload.creators.clone(),
                payload.year,
                payload.doi.clone(),
            )
            .map(|_| ()),
        }
    }
}

/// One exact canonical portable record at a zero-based package record ordinal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableRecord {
    record_ordinal: U64Decimal,
    subject_ids: Vec<TypedId>,
    payload: PortableRecordPayload,
}

impl PortableRecord {
    pub fn new(
        record_ordinal: U64Decimal,
        subject_ids: Vec<TypedId>,
        payload: PortableRecordPayload,
    ) -> Result<Self, ControlError> {
        validate_record_ordinal(record_ordinal)?;
        validate_subject_ids(&subject_ids)?;
        payload.validate()?;
        let record = Self {
            record_ordinal,
            subject_ids,
            payload,
        };
        record.canonical_bytes()?;
        Ok(record)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WirePortableRecord = serde_json::from_slice(bytes).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        let value = Self::try_from(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject { object: OBJECT });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        validate_record_ordinal(self.record_ordinal)?;
        validate_subject_ids(&self.subject_ids)?;
        self.payload.validate()?;
        let value = serde_json::to_value(WirePortableRecord::from(self)).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        jcs::encode(&value, OBJECT, MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub const fn record_ordinal(&self) -> U64Decimal {
        self.record_ordinal
    }

    pub fn subject_ids(&self) -> &[TypedId] {
        &self.subject_ids
    }

    pub const fn kind(&self) -> PortableRecordKind {
        self.payload.kind()
    }

    pub const fn payload(&self) -> &PortableRecordPayload {
        &self.payload
    }
}

fn validate_record_ordinal(ordinal: U64Decimal) -> Result<(), ControlError> {
    if ordinal.get() >= PORTABLE_RECORD_COUNT_MAX as u64 {
        return invalid("portable record ordinal must be in zero through thirteen");
    }
    Ok(())
}

fn validate_subject_ids(subject_ids: &[TypedId]) -> Result<(), ControlError> {
    if subject_ids.is_empty() || subject_ids.len() > CONTROL_COLLECTION_ITEMS_MAX {
        return invalid("portable record subject_ids must be nonempty");
    }
    let mut previous: Option<String> = None;
    for subject_id in subject_ids {
        let current = subject_id.to_string();
        if previous
            .as_deref()
            .is_some_and(|previous| previous.as_bytes() >= current.as_bytes())
        {
            return invalid("portable record subject_ids must be strictly ASCII-sorted and unique");
        }
        previous = Some(current);
    }
    Ok(())
}

fn validate_source_identifiers(identifiers: &[SourceIdentifier]) -> Result<(), ControlError> {
    if identifiers.is_empty() || identifiers.len() > CONTROL_COLLECTION_ITEMS_MAX {
        return invalid("source_identifiers must be nonempty");
    }
    validate_text_budget(
        identifiers
            .iter()
            .map(|identifier| identifier.value.as_str().len()),
    )?;
    if !identifiers.windows(2).all(|pair| {
        (
            pair[0].scheme.as_str().as_bytes(),
            pair[0].value.as_str().as_bytes(),
        ) < (
            pair[1].scheme.as_str().as_bytes(),
            pair[1].value.as_str().as_bytes(),
        )
    }) {
        return invalid("source_identifiers must be strictly sorted and unique");
    }
    Ok(())
}

fn validate_nfc_set(values: &[NfcText], label: &'static str) -> Result<(), ControlError> {
    if values.is_empty()
        || values.len() > CONTROL_COLLECTION_ITEMS_MAX
        || values.iter().any(|value| value.as_str().is_empty())
    {
        return invalid(match label {
            "rights holders" => "rights holders must be nonempty text values",
            _ => "NFC set values must be nonempty",
        });
    }
    if !values
        .windows(2)
        .all(|pair| pair[0].as_str().as_bytes() < pair[1].as_str().as_bytes())
    {
        return invalid(match label {
            "rights holders" => "rights holders must be strictly UTF-8-byte-sorted and unique",
            _ => "NFC set values must be strictly UTF-8-byte-sorted and unique",
        });
    }
    validate_text_budget(values.iter().map(|value| value.as_str().len()))?;
    Ok(())
}

fn validate_text_budget(lengths: impl IntoIterator<Item = usize>) -> Result<(), ControlError> {
    let bytes = lengths
        .into_iter()
        .try_fold(0_usize, |total, length| total.checked_add(length))
        .ok_or(ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "portable record text byte count overflowed usize",
        })?;
    if bytes > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
        return Err(ControlError::ControlObjectTooLarge {
            object: OBJECT,
            maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
        });
    }
    Ok(())
}

fn invalid<T>(reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidControlObject {
        object: OBJECT,
        reason,
    })
}

fn invalid_scalar<T>(kind: &'static str, reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidScalar { kind, reason })
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum WirePortableRecord {
    #[serde(rename = "source")]
    Source {
        schema: String,
        schema_version: u64,
        record_ordinal: String,
        subject_ids: Vec<String>,
        payload: WireSourcePayload,
    },
    #[serde(rename = "recipe")]
    Recipe {
        schema: String,
        schema_version: u64,
        record_ordinal: String,
        subject_ids: Vec<String>,
        payload: WireRecipePayload,
    },
    #[serde(rename = "derivation")]
    Derivation {
        schema: String,
        schema_version: u64,
        record_ordinal: String,
        subject_ids: Vec<String>,
        payload: WireDerivationPayload,
    },
    #[serde(rename = "rights")]
    Rights {
        schema: String,
        schema_version: u64,
        record_ordinal: String,
        subject_ids: Vec<String>,
        payload: WireRightsPayload,
    },
    #[serde(rename = "citation")]
    Citation {
        schema: String,
        schema_version: u64,
        record_ordinal: String,
        subject_ids: Vec<String>,
        payload: WireCitationPayload,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourcePayload {
    source_identifiers: Vec<WireSourceIdentifier>,
    parent_release_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourceIdentifier {
    scheme: String,
    value: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRightsPayload {
    license_spdx: String,
    rights_holders: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCitationPayload {
    title: String,
    creators: Vec<String>,
    year: Option<String>,
    doi: Option<String>,
}

impl TryFrom<WirePortableRecord> for PortableRecord {
    type Error = ControlError;

    fn try_from(wire: WirePortableRecord) -> Result<Self, Self::Error> {
        match wire {
            WirePortableRecord::Source {
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                payload,
            } => Self::try_from_wire_parts(
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                PortableRecordPayload::Source(SourcePayload::try_from(payload)?),
            ),
            WirePortableRecord::Recipe {
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                payload,
            } => Self::try_from_wire_parts(
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                PortableRecordPayload::Recipe(RecipePayload::try_from_wire(payload)?),
            ),
            WirePortableRecord::Derivation {
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                payload,
            } => Self::try_from_wire_parts(
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                PortableRecordPayload::Derivation(DerivationPayload::try_from_wire(payload)?),
            ),
            WirePortableRecord::Rights {
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                payload,
            } => Self::try_from_wire_parts(
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                PortableRecordPayload::Rights(RightsPayload::try_from(payload)?),
            ),
            WirePortableRecord::Citation {
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                payload,
            } => Self::try_from_wire_parts(
                schema,
                schema_version,
                record_ordinal,
                subject_ids,
                PortableRecordPayload::Citation(CitationPayload::try_from(payload)?),
            ),
        }
    }
}

impl PortableRecord {
    fn try_from_wire_parts(
        schema: String,
        schema_version: u64,
        record_ordinal: String,
        subject_ids: Vec<String>,
        payload: PortableRecordPayload,
    ) -> Result<Self, ControlError> {
        if schema != SCHEMA || schema_version != 1 {
            return invalid("portable record schema or version is unsupported");
        }
        Self::new(
            U64Decimal::parse(&record_ordinal)?,
            subject_ids
                .into_iter()
                .map(|subject_id| TypedId::parse(&subject_id))
                .collect::<Result<Vec<_>, _>>()?,
            payload,
        )
    }
}

impl TryFrom<WireSourcePayload> for SourcePayload {
    type Error = ControlError;

    fn try_from(wire: WireSourcePayload) -> Result<Self, Self::Error> {
        Self::new(
            wire.source_identifiers
                .into_iter()
                .map(SourceIdentifier::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            wire.parent_release_id
                .map(|release_id| {
                    ReleaseId::parse(&release_id).map_err(|_| ControlError::InvalidControlObject {
                        object: OBJECT,
                        reason: "parent_release_id is invalid",
                    })
                })
                .transpose()?,
        )
    }
}

impl TryFrom<WireSourceIdentifier> for SourceIdentifier {
    type Error = ControlError;

    fn try_from(wire: WireSourceIdentifier) -> Result<Self, Self::Error> {
        Self::new(
            SourceIdentifierScheme::parse(&wire.scheme)?,
            NfcText::parse(&wire.value)?,
        )
    }
}

impl TryFrom<WireRightsPayload> for RightsPayload {
    type Error = ControlError;

    fn try_from(wire: WireRightsPayload) -> Result<Self, Self::Error> {
        Self::new(
            SpdxLicense::parse(&wire.license_spdx)?,
            wire.rights_holders
                .into_iter()
                .map(|holder| NfcText::parse(&holder))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl TryFrom<WireCitationPayload> for CitationPayload {
    type Error = ControlError;

    fn try_from(wire: WireCitationPayload) -> Result<Self, Self::Error> {
        Self::new(
            NfcText::parse(&wire.title)?,
            wire.creators
                .into_iter()
                .map(|creator| NfcText::parse(&creator))
                .collect::<Result<Vec<_>, _>>()?,
            wire.year.map(|year| U64Decimal::parse(&year)).transpose()?,
            wire.doi.map(|doi| Doi::parse(&doi)).transpose()?,
        )
    }
}

impl From<&PortableRecord> for WirePortableRecord {
    fn from(value: &PortableRecord) -> Self {
        let subject_ids = || value.subject_ids.iter().map(ToString::to_string).collect();
        match &value.payload {
            PortableRecordPayload::Source(payload) => Self::Source {
                schema: SCHEMA.to_owned(),
                schema_version: 1,
                record_ordinal: value.record_ordinal.to_string(),
                subject_ids: subject_ids(),
                payload: WireSourcePayload::from(payload),
            },
            PortableRecordPayload::Recipe(payload) => Self::Recipe {
                schema: SCHEMA.to_owned(),
                schema_version: 1,
                record_ordinal: value.record_ordinal.to_string(),
                subject_ids: subject_ids(),
                payload: WireRecipePayload::from(payload),
            },
            PortableRecordPayload::Derivation(payload) => Self::Derivation {
                schema: SCHEMA.to_owned(),
                schema_version: 1,
                record_ordinal: value.record_ordinal.to_string(),
                subject_ids: subject_ids(),
                payload: WireDerivationPayload::from(payload),
            },
            PortableRecordPayload::Rights(payload) => Self::Rights {
                schema: SCHEMA.to_owned(),
                schema_version: 1,
                record_ordinal: value.record_ordinal.to_string(),
                subject_ids: subject_ids(),
                payload: WireRightsPayload::from(payload),
            },
            PortableRecordPayload::Citation(payload) => Self::Citation {
                schema: SCHEMA.to_owned(),
                schema_version: 1,
                record_ordinal: value.record_ordinal.to_string(),
                subject_ids: subject_ids(),
                payload: WireCitationPayload::from(payload),
            },
        }
    }
}

impl From<&SourcePayload> for WireSourcePayload {
    fn from(value: &SourcePayload) -> Self {
        Self {
            source_identifiers: value
                .source_identifiers
                .iter()
                .map(WireSourceIdentifier::from)
                .collect(),
            parent_release_id: value.parent_release_id.map(|value| value.to_string()),
        }
    }
}

impl From<&SourceIdentifier> for WireSourceIdentifier {
    fn from(value: &SourceIdentifier) -> Self {
        Self {
            scheme: value.scheme.to_string(),
            value: value.value.to_string(),
        }
    }
}

impl From<&RightsPayload> for WireRightsPayload {
    fn from(value: &RightsPayload) -> Self {
        Self {
            license_spdx: value.license_spdx.to_string(),
            rights_holders: value
                .rights_holders
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

impl From<&CitationPayload> for WireCitationPayload {
    fn from(value: &CitationPayload) -> Self {
        Self {
            title: value.title.to_string(),
            creators: value.creators.iter().map(ToString::to_string).collect(),
            year: value.year.map(|year| year.to_string()),
            doi: value.doi.as_ref().map(ToString::to_string),
        }
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_identity::{ExactBytesDigest, PackageId, RecipeId};

    use super::super::{
        AsciiToken, CanonicalValue, DerivationBinding, DerivationBody, DerivationExactness,
        DerivationImplementation, DerivationOutcome, DerivationScope, DerivationSpaceBox,
        DerivationTimeRange, RecipeBody, RecipeDeterminism, RecipeNumericPolicy, RecipeOperation,
    };
    use super::*;

    fn text(value: &str) -> NfcText {
        NfcText::parse(value).unwrap()
    }

    fn number(value: &str) -> U64Decimal {
        U64Decimal::parse(value).unwrap()
    }

    fn subject(hex: char) -> TypedId {
        TypedId::parse(&format!("m4d-sc-v1-sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn token(value: &str) -> AsciiToken {
        AsciiToken::parse(value).unwrap()
    }

    fn derivation_with_output(id: TypedId) -> DerivationPayload {
        let zero = number("0");
        let one = number("1");
        DerivationPayload::new(
            DerivationBody::new(
                RecipeId::parse(&format!("{}{}", RecipeId::PREFIX, "0".repeat(64))).unwrap(),
                Vec::new(),
                vec![DerivationBinding::new(token("result"), id)],
                DerivationScope::new(
                    vec![zero],
                    vec![DerivationTimeRange::new(zero, zero).unwrap()],
                    vec![DerivationSpaceBox::new([zero; 4], [one; 4]).unwrap()],
                )
                .unwrap(),
                DerivationImplementation::new(
                    token("mirante4d"),
                    token("0.1.0"),
                    ExactBytesDigest::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
                ),
                DerivationOutcome::Success,
                DerivationExactness::Exact,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn recipe() -> RecipePayload {
        RecipePayload::new(
            RecipeBody::new(
                ExactBytesDigest::parse(&format!("sha256:{}", "2".repeat(64))).unwrap(),
                RecipeDeterminism::BitExact,
                vec![
                    RecipeOperation::new(
                        number("0"),
                        token("identity"),
                        token("1.0.0"),
                        token("m4d.params.v1"),
                        CanonicalValue::from_bool(true),
                        Vec::new(),
                        RecipeNumericPolicy::new(
                            token("uint16"),
                            token("nearest"),
                            token("none"),
                            token("identity"),
                            token("error"),
                            token("none"),
                            token("error"),
                            token("tzyx"),
                            token("exact"),
                            None,
                        ),
                        vec![token("result")],
                    )
                    .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn source_rights_and_citation_records_roundtrip_exact_bytes() {
        let source = PortableRecord::new(
            number("0"),
            vec![subject('0')],
            PortableRecordPayload::Source(
                SourcePayload::new(
                    vec![
                        SourceIdentifier::new(
                            SourceIdentifierScheme::Doi,
                            text("10.64898/2025.12.31.697247v2"),
                        )
                        .unwrap(),
                    ],
                    None,
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let expected = r#"{"kind":"source","payload":{"parent_release_id":null,"source_identifiers":[{"scheme":"doi","value":"10.64898/2025.12.31.697247v2"}]},"record_ordinal":"0","schema":"m4d-portable-record","schema_version":1,"subject_ids":["m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"]}"#;
        assert_eq!(source.canonical_bytes().unwrap(), expected.as_bytes());
        assert_eq!(
            PortableRecord::parse_canonical(expected.as_bytes()).unwrap(),
            source
        );

        for record in [
            PortableRecord::new(
                number("1"),
                vec![subject('0')],
                PortableRecordPayload::Rights(
                    RightsPayload::new(SpdxLicense::CcBy40, vec![text("Kirchhausen Lab")]).unwrap(),
                ),
            )
            .unwrap(),
            PortableRecord::new(
                number("2"),
                vec![subject('0')],
                PortableRecordPayload::Citation(
                    CitationPayload::new(
                        text("SpatialDINO"),
                        vec![text("First Creator"), text("First Creator")],
                        Some(number("2025")),
                        Some(Doi::parse("10.64898/2025.12.31.697247v2").unwrap()),
                    )
                    .unwrap(),
                ),
            )
            .unwrap(),
        ] {
            let bytes = record.canonical_bytes().unwrap();
            assert_eq!(PortableRecord::parse_canonical(&bytes).unwrap(), record);
        }
    }

    #[test]
    fn portable_records_reject_invalid_scalars_order_shapes_and_encoding() {
        for invalid in [
            "https://doi.org/10.64898/example",
            "10.123/example",
            "10.1234/Upper",
            "10.1234/",
        ] {
            assert!(Doi::parse(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(SpdxLicense::parse("CC-BY-SA-4.0").is_err());
        assert!(SourceIdentifier::new(SourceIdentifierScheme::Sha256, text("00")).is_err());

        let doi =
            SourceIdentifier::new(SourceIdentifierScheme::Doi, text("10.1234/example")).unwrap();
        let sha =
            SourceIdentifier::new(SourceIdentifierScheme::Sha256, text(&"0".repeat(64))).unwrap();
        assert!(SourcePayload::new(vec![sha, doi], None).is_err());
        assert!(RightsPayload::new(SpdxLicense::Mit, vec![text("b"), text("a")]).is_err());
        assert!(CitationPayload::new(text(""), vec![text("creator")], None, None).is_err());
        assert!(
            CitationPayload::new(
                text("title"),
                vec![text("creator")],
                Some(number("999")),
                None,
            )
            .is_err()
        );

        let payload = PortableRecordPayload::Rights(
            RightsPayload::new(SpdxLicense::Mit, vec![text("holder")]).unwrap(),
        );
        assert!(PortableRecord::new(number("14"), vec![subject('0')], payload.clone()).is_err());
        assert!(
            PortableRecord::new(number("0"), vec![subject('1'), subject('0')], payload,).is_err()
        );

        let canonical = PortableRecord::new(
            number("0"),
            vec![subject('0')],
            PortableRecordPayload::Rights(
                RightsPayload::new(SpdxLicense::Mit, vec![text("holder")]).unwrap(),
            ),
        )
        .unwrap()
        .canonical_bytes()
        .unwrap();
        let duplicate_payload = String::from_utf8(canonical.clone()).unwrap().replacen(
            "\"license_spdx\":",
            "\"license_spdx\":\"MIT\",\"license_spdx\":",
            1,
        );
        let unknown_payload = String::from_utf8(canonical.clone()).unwrap().replacen(
            "\"license_spdx\":",
            "\"extra\":false,\"license_spdx\":",
            1,
        );
        for bytes in [
            duplicate_payload.into_bytes(),
            unknown_payload.into_bytes(),
            [b" ".as_slice(), canonical.as_slice()].concat(),
        ] {
            assert!(PortableRecord::parse_canonical(&bytes).is_err());
        }

        let package_output = derivation_with_output(TypedId::Package(
            PackageId::parse(&format!("{}{}", PackageId::PREFIX, "3".repeat(64))).unwrap(),
        ));
        assert!(package_output.canonical_bytes().is_ok());
        assert!(
            PortableRecord::new(
                number("3"),
                vec![subject('0')],
                PortableRecordPayload::Derivation(package_output.clone()),
            )
            .is_err()
        );
        let package_wire = WirePortableRecord::Derivation {
            schema: SCHEMA.to_owned(),
            schema_version: 1,
            record_ordinal: "3".to_owned(),
            subject_ids: vec![subject('0').to_string()],
            payload: WireDerivationPayload::from(&package_output),
        };
        let package_bytes = jcs::encode(
            &serde_json::to_value(package_wire).unwrap(),
            OBJECT,
            MAX_PORTABLE_CONTROL_OBJECT_BYTES,
        )
        .unwrap();
        assert!(PortableRecord::parse_canonical(&package_bytes).is_err());

        let artifact_output = derivation_with_output(
            TypedId::parse(&format!("m4d-artifact-v1-sha256:{}", "4".repeat(64))).unwrap(),
        );
        for record in [
            PortableRecord::new(
                number("3"),
                vec![subject('0')],
                PortableRecordPayload::Derivation(artifact_output),
            )
            .unwrap(),
            PortableRecord::new(
                number("4"),
                vec![subject('0')],
                PortableRecordPayload::Recipe(recipe()),
            )
            .unwrap(),
        ] {
            let bytes = record.canonical_bytes().unwrap();
            assert_eq!(PortableRecord::parse_canonical(&bytes).unwrap(), record);
        }
    }
}
