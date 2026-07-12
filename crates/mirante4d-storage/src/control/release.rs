use std::{fmt, str::FromStr};

use mirante4d_identity::{
    DerivationRecordId, ExactBytesDigest, PackageId, RecipeId, ReleaseId, ScientificContentId,
};
use serde::{Deserialize, Serialize};

use super::{
    AsciiToken, CONTROL_COLLECTION_ITEMS_MAX, ControlError, MAX_PORTABLE_CONTROL_OBJECT_BYTES,
    NfcText, U64Decimal, jcs,
    portable::{Doi, SpdxLicense},
};

const OBJECT: &str = "release record";
const BODY_OBJECT: &str = "release body";
const SCHEMA: &str = "m4d-release";

/// A canonical version-4 dataset-series UUID under the release profile.
/// Random generation is the curator's responsibility, not a parsing claim.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DatasetSeriesUuid(String);

impl DatasetSeriesUuid {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for DatasetSeriesUuid {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != 36
            || !value.is_ascii()
            || bytes[8] != b'-'
            || bytes[13] != b'-'
            || bytes[18] != b'-'
            || bytes[23] != b'-'
            || bytes
                .iter()
                .enumerate()
                .filter(|(index, _)| !matches!(index, 8 | 13 | 18 | 23))
                .any(|(_, byte)| !byte.is_ascii_digit() && !matches!(*byte, b'a'..=b'f'))
            || bytes[14] != b'4'
            || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        {
            return invalid_scalar(
                "dataset series UUID",
                "expected a lowercase hyphenated RFC 4122 version-4 UUID",
            );
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for DatasetSeriesUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A canonical whole-second UTC publication timestamp.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublishedAtUtc(String);

impl PublishedAtUtc {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PublishedAtUtc {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != 20
            || !value.is_ascii()
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
            || bytes[19] != b'Z'
        {
            return invalid_timestamp();
        }
        let year = decimal_field(bytes, 0, 4).ok_or_else(timestamp_error)?;
        let month = decimal_field(bytes, 5, 2).ok_or_else(timestamp_error)?;
        let day = decimal_field(bytes, 8, 2).ok_or_else(timestamp_error)?;
        let hour = decimal_field(bytes, 11, 2).ok_or_else(timestamp_error)?;
        let minute = decimal_field(bytes, 14, 2).ok_or_else(timestamp_error)?;
        let second = decimal_field(bytes, 17, 2).ok_or_else(timestamp_error)?;
        if year == 0
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return invalid_timestamp();
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for PublishedAtUtc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Citation fields stored once in a version-1 release body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCitation {
    title: NfcText,
    year: Option<U64Decimal>,
    doi: Option<Doi>,
}

impl ReleaseCitation {
    pub fn new(
        title: NfcText,
        year: Option<U64Decimal>,
        doi: Option<Doi>,
    ) -> Result<Self, ControlError> {
        if title.as_str().is_empty() {
            return invalid("release citation title must be nonempty");
        }
        if year.is_some_and(|year| !(1_000..=9_999).contains(&year.get())) {
            return invalid("release citation year must be null or in 1000 through 9999");
        }
        Ok(Self { title, year, doi })
    }

    pub const fn title(&self) -> &NfcText {
        &self.title
    }

    pub const fn year(&self) -> Option<U64Decimal> {
        self.year
    }

    pub const fn doi(&self) -> Option<&Doi> {
        self.doi.as_ref()
    }
}

/// One typed evidence digest in a version-1 release body.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseEvidence {
    kind: AsciiToken,
    digest: ExactBytesDigest,
}

impl ReleaseEvidence {
    pub const fn new(kind: AsciiToken, digest: ExactBytesDigest) -> Self {
        Self { kind, digest }
    }

    pub const fn kind(&self) -> &AsciiToken {
        &self.kind
    }

    pub const fn digest(&self) -> ExactBytesDigest {
        self.digest
    }
}

/// The identity-bearing canonical body of an immutable dataset release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseBody {
    dataset_series_uuid: DatasetSeriesUuid,
    release_ordinal: U64Decimal,
    scientific_content_id: ScientificContentId,
    package_id: PackageId,
    recipe_ids: Vec<RecipeId>,
    derivation_record_ids: Vec<DerivationRecordId>,
    portable_record_digests: Vec<ExactBytesDigest>,
    schema_profiles: Vec<AsciiToken>,
    license_spdx: SpdxLicense,
    rights_holders: Vec<NfcText>,
    citation: ReleaseCitation,
    creators: Vec<NfcText>,
    institutions: Vec<NfcText>,
    funders: Vec<NfcText>,
    evidence: Vec<ReleaseEvidence>,
    published_at: PublishedAtUtc,
    supersedes: Vec<ReleaseId>,
}

impl ReleaseBody {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dataset_series_uuid: DatasetSeriesUuid,
        release_ordinal: U64Decimal,
        scientific_content_id: ScientificContentId,
        package_id: PackageId,
        recipe_ids: Vec<RecipeId>,
        derivation_record_ids: Vec<DerivationRecordId>,
        portable_record_digests: Vec<ExactBytesDigest>,
        schema_profiles: Vec<AsciiToken>,
        license_spdx: SpdxLicense,
        rights_holders: Vec<NfcText>,
        citation: ReleaseCitation,
        creators: Vec<NfcText>,
        institutions: Vec<NfcText>,
        funders: Vec<NfcText>,
        evidence: Vec<ReleaseEvidence>,
        published_at: PublishedAtUtc,
        supersedes: Vec<ReleaseId>,
    ) -> Result<Self, ControlError> {
        let body = Self {
            dataset_series_uuid,
            release_ordinal,
            scientific_content_id,
            package_id,
            recipe_ids,
            derivation_record_ids,
            portable_record_digests,
            schema_profiles,
            license_spdx,
            rights_holders,
            citation,
            creators,
            institutions,
            funders,
            evidence,
            published_at,
            supersedes,
        };
        body.validate()?;
        body.canonical_bytes()?;
        Ok(body)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        self.validate()?;
        encode_wire(
            WireReleaseBody::from(self),
            BODY_OBJECT,
            MAX_PORTABLE_CONTROL_OBJECT_BYTES,
        )
    }

    pub const fn dataset_series_uuid(&self) -> &DatasetSeriesUuid {
        &self.dataset_series_uuid
    }

    pub const fn release_ordinal(&self) -> U64Decimal {
        self.release_ordinal
    }

    pub const fn scientific_content_id(&self) -> ScientificContentId {
        self.scientific_content_id
    }

    pub const fn package_id(&self) -> PackageId {
        self.package_id
    }

    pub fn recipe_ids(&self) -> &[RecipeId] {
        &self.recipe_ids
    }

    pub fn derivation_record_ids(&self) -> &[DerivationRecordId] {
        &self.derivation_record_ids
    }

    pub fn portable_record_digests(&self) -> &[ExactBytesDigest] {
        &self.portable_record_digests
    }

    pub fn schema_profiles(&self) -> &[AsciiToken] {
        &self.schema_profiles
    }

    pub const fn license_spdx(&self) -> SpdxLicense {
        self.license_spdx
    }

    pub fn rights_holders(&self) -> &[NfcText] {
        &self.rights_holders
    }

    pub const fn citation(&self) -> &ReleaseCitation {
        &self.citation
    }

    pub fn creators(&self) -> &[NfcText] {
        &self.creators
    }

    pub fn institutions(&self) -> &[NfcText] {
        &self.institutions
    }

    pub fn funders(&self) -> &[NfcText] {
        &self.funders
    }

    pub fn evidence(&self) -> &[ReleaseEvidence] {
        &self.evidence
    }

    pub const fn published_at(&self) -> &PublishedAtUtc {
        &self.published_at
    }

    pub fn supersedes(&self) -> &[ReleaseId] {
        &self.supersedes
    }

    fn validate(&self) -> Result<(), ControlError> {
        self.validate_preflight()?;
        if self.release_ordinal.get() == 0 {
            return invalid("release ordinal must be positive");
        }
        require_sorted(&self.recipe_ids, false, "recipe_ids")?;
        require_sorted(&self.derivation_record_ids, false, "derivation_record_ids")?;
        require_sorted(
            &self.portable_record_digests,
            false,
            "portable_record_digests",
        )?;
        require_sorted(&self.schema_profiles, true, "schema_profiles")?;
        require_nfc_set(&self.rights_holders, true, "rights_holders")?;
        ReleaseCitation::new(
            self.citation.title.clone(),
            self.citation.year,
            self.citation.doi.clone(),
        )?;
        if self.creators.is_empty()
            || self
                .creators
                .iter()
                .any(|creator| creator.as_str().is_empty())
        {
            return invalid("release creators must be nonempty text values");
        }
        require_nfc_set(&self.institutions, false, "institutions")?;
        require_nfc_set(&self.funders, false, "funders")?;
        require_sorted(&self.evidence, false, "evidence")?;
        require_sorted(&self.supersedes, false, "supersedes")
    }

    fn validate_preflight(&self) -> Result<(), ControlError> {
        let item_count = [
            self.recipe_ids.len(),
            self.derivation_record_ids.len(),
            self.portable_record_digests.len(),
            self.schema_profiles.len(),
            self.rights_holders.len(),
            self.creators.len(),
            self.institutions.len(),
            self.funders.len(),
            self.evidence.len(),
            self.supersedes.len(),
        ]
        .into_iter()
        .try_fold(0_usize, |total, count| total.checked_add(count))
        .ok_or(ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "release collection count overflowed usize",
        })?;
        if item_count > CONTROL_COLLECTION_ITEMS_MAX {
            return invalid("release collection items exceed 4096");
        }

        let variable_text_bytes = self
            .schema_profiles
            .iter()
            .map(|value| value.as_str().len())
            .chain(self.rights_holders.iter().map(|value| value.as_str().len()))
            .chain(self.creators.iter().map(|value| value.as_str().len()))
            .chain(self.institutions.iter().map(|value| value.as_str().len()))
            .chain(self.funders.iter().map(|value| value.as_str().len()))
            .chain(self.evidence.iter().map(|value| value.kind.as_str().len()))
            .chain(std::iter::once(self.citation.title.as_str().len()))
            .chain(self.citation.doi.iter().map(|value| value.as_str().len()))
            .try_fold(0_usize, |total, bytes| total.checked_add(bytes))
            .ok_or(ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "release text byte count overflowed usize",
            })?;
        if variable_text_bytes > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        Ok(())
    }
}

/// A canonical release body bound to its verified version-1 ReleaseId.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasePayload {
    release_id: ReleaseId,
    body: ReleaseBody,
}

impl ReleasePayload {
    pub fn new(body: ReleaseBody) -> Result<Self, ControlError> {
        let body_bytes = body.canonical_bytes()?;
        let release_id = ReleaseId::from_canonical_body_bytes(&body_bytes).map_err(|_| {
            ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "release body length exceeds identity framing",
            }
        })?;
        let payload = Self { release_id, body };
        payload.validate_identity()?;
        payload.canonical_bytes()?;
        Ok(payload)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireReleasePayload = serde_json::from_slice(bytes).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        if wire.schema != SCHEMA || wire.schema_version != 1 {
            return invalid("release schema or version is unsupported");
        }
        let declared_id =
            ReleaseId::parse(&wire.release_id).map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "release_id is invalid",
            })?;
        let payload = Self::new(ReleaseBody::try_from(wire.body)?)?;
        if payload.release_id != declared_id {
            return invalid("release_id does not verify the canonical body");
        }
        if payload.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject { object: OBJECT });
        }
        Ok(payload)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        self.validate_identity()?;
        encode_wire(
            WireReleasePayload::from(self),
            OBJECT,
            MAX_PORTABLE_CONTROL_OBJECT_BYTES,
        )
    }

    pub const fn release_id(&self) -> ReleaseId {
        self.release_id
    }

    pub const fn body(&self) -> &ReleaseBody {
        &self.body
    }

    fn validate_identity(&self) -> Result<(), ControlError> {
        self.body.validate()?;
        if self.body.supersedes.binary_search(&self.release_id).is_ok() {
            return invalid("a release cannot supersede its own release_id");
        }
        let body_bytes = self.body.canonical_bytes()?;
        if !self
            .release_id
            .matches_canonical_body_bytes(&body_bytes)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "release body length exceeds identity framing",
            })?
        {
            return invalid("release_id does not verify the canonical body");
        }
        Ok(())
    }
}

fn require_sorted<T: Ord>(
    values: &[T],
    nonempty: bool,
    label: &'static str,
) -> Result<(), ControlError> {
    if nonempty && values.is_empty() {
        return invalid(match label {
            "schema_profiles" => "schema_profiles must be nonempty",
            _ => "release set must be nonempty",
        });
    }
    if !values.windows(2).all(|pair| pair[0] < pair[1]) {
        return invalid(match label {
            "recipe_ids" => "recipe_ids must be strictly ASCII-sorted and unique",
            "derivation_record_ids" => {
                "derivation_record_ids must be strictly ASCII-sorted and unique"
            }
            "portable_record_digests" => {
                "portable_record_digests must be strictly ASCII-sorted and unique"
            }
            "schema_profiles" => "schema_profiles must be strictly ASCII-sorted and unique",
            "evidence" => "evidence must be strictly sorted and unique by kind then digest",
            "supersedes" => "supersedes must be strictly ASCII-sorted and unique",
            _ => "release set must be strictly sorted and unique",
        });
    }
    Ok(())
}

fn require_nfc_set(
    values: &[NfcText],
    nonempty: bool,
    label: &'static str,
) -> Result<(), ControlError> {
    if (nonempty && values.is_empty()) || values.iter().any(|value| value.as_str().is_empty()) {
        return invalid(match label {
            "rights_holders" => "rights_holders must be nonempty text values",
            "institutions" => "institution values must be nonempty",
            "funders" => "funder values must be nonempty",
            _ => "release NFC-set values must be nonempty",
        });
    }
    if !values
        .windows(2)
        .all(|pair| pair[0].as_str().as_bytes() < pair[1].as_str().as_bytes())
    {
        return invalid(match label {
            "rights_holders" => "rights_holders must be strictly UTF-8-byte-sorted and unique",
            "institutions" => "institutions must be strictly UTF-8-byte-sorted and unique",
            "funders" => "funders must be strictly UTF-8-byte-sorted and unique",
            _ => "release NFC set must be strictly UTF-8-byte-sorted and unique",
        });
    }
    Ok(())
}

fn decimal_field(bytes: &[u8], start: usize, width: usize) -> Option<u32> {
    bytes
        .get(start..start.checked_add(width)?)?
        .iter()
        .try_fold(0_u32, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(*byte - b'0'))
        })
}

const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn invalid_timestamp<T>() -> Result<T, ControlError> {
    Err(timestamp_error())
}

const fn timestamp_error() -> ControlError {
    ControlError::InvalidScalar {
        kind: "publication timestamp",
        reason: "expected a valid whole-second UTC Gregorian YYYY-MM-DDTHH:MM:SSZ value",
    }
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

fn encode_wire<T: Serialize>(
    wire: T,
    object: &'static str,
    maximum: usize,
) -> Result<Vec<u8>, ControlError> {
    let value =
        serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
            object,
            detail: error.to_string(),
        })?;
    jcs::encode(&value, object, maximum)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReleasePayload {
    schema: String,
    schema_version: u64,
    release_id: String,
    body: WireReleaseBody,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReleaseBody {
    dataset_series_uuid: String,
    release_ordinal: String,
    scientific_content_id: String,
    package_id: String,
    recipe_ids: Vec<String>,
    derivation_record_ids: Vec<String>,
    portable_record_digests: Vec<String>,
    schema_profiles: Vec<String>,
    license_spdx: String,
    rights_holders: Vec<String>,
    citation: WireReleaseCitation,
    creators: Vec<String>,
    institutions: Vec<String>,
    funders: Vec<String>,
    evidence: Vec<WireReleaseEvidence>,
    published_at: String,
    supersedes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReleaseCitation {
    title: String,
    year: Option<String>,
    doi: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReleaseEvidence {
    kind: String,
    digest: String,
}

impl TryFrom<WireReleaseBody> for ReleaseBody {
    type Error = ControlError;

    fn try_from(wire: WireReleaseBody) -> Result<Self, Self::Error> {
        Self::new(
            DatasetSeriesUuid::parse(&wire.dataset_series_uuid)?,
            U64Decimal::parse(&wire.release_ordinal)?,
            ScientificContentId::parse(&wire.scientific_content_id).map_err(|_| {
                ControlError::InvalidControlObject {
                    object: OBJECT,
                    reason: "scientific_content_id is invalid",
                }
            })?,
            PackageId::parse(&wire.package_id).map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "package_id is invalid",
            })?,
            wire.recipe_ids
                .into_iter()
                .map(|value| {
                    RecipeId::parse(&value).map_err(|_| ControlError::InvalidControlObject {
                        object: OBJECT,
                        reason: "a recipe_id is invalid",
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            wire.derivation_record_ids
                .into_iter()
                .map(|value| {
                    DerivationRecordId::parse(&value).map_err(|_| {
                        ControlError::InvalidControlObject {
                            object: OBJECT,
                            reason: "a derivation_record_id is invalid",
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            wire.portable_record_digests
                .into_iter()
                .map(|value| {
                    ExactBytesDigest::parse(&value).map_err(|_| {
                        ControlError::InvalidControlObject {
                            object: OBJECT,
                            reason: "a portable record digest is invalid",
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            wire.schema_profiles
                .into_iter()
                .map(|value| AsciiToken::parse(&value))
                .collect::<Result<Vec<_>, _>>()?,
            SpdxLicense::parse(&wire.license_spdx)?,
            wire.rights_holders
                .into_iter()
                .map(|value| NfcText::parse(&value))
                .collect::<Result<Vec<_>, _>>()?,
            ReleaseCitation::try_from(wire.citation)?,
            wire.creators
                .into_iter()
                .map(|value| NfcText::parse(&value))
                .collect::<Result<Vec<_>, _>>()?,
            wire.institutions
                .into_iter()
                .map(|value| NfcText::parse(&value))
                .collect::<Result<Vec<_>, _>>()?,
            wire.funders
                .into_iter()
                .map(|value| NfcText::parse(&value))
                .collect::<Result<Vec<_>, _>>()?,
            wire.evidence
                .into_iter()
                .map(ReleaseEvidence::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            PublishedAtUtc::parse(&wire.published_at)?,
            wire.supersedes
                .into_iter()
                .map(|value| {
                    ReleaseId::parse(&value).map_err(|_| ControlError::InvalidControlObject {
                        object: OBJECT,
                        reason: "a superseded release_id is invalid",
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl TryFrom<WireReleaseCitation> for ReleaseCitation {
    type Error = ControlError;

    fn try_from(wire: WireReleaseCitation) -> Result<Self, Self::Error> {
        Self::new(
            NfcText::parse(&wire.title)?,
            wire.year
                .map(|value| U64Decimal::parse(&value))
                .transpose()?,
            wire.doi.map(|value| Doi::parse(&value)).transpose()?,
        )
    }
}

impl TryFrom<WireReleaseEvidence> for ReleaseEvidence {
    type Error = ControlError;

    fn try_from(wire: WireReleaseEvidence) -> Result<Self, Self::Error> {
        Ok(Self::new(
            AsciiToken::parse(&wire.kind)?,
            ExactBytesDigest::parse(&wire.digest).map_err(|_| {
                ControlError::InvalidControlObject {
                    object: OBJECT,
                    reason: "release evidence digest is invalid",
                }
            })?,
        ))
    }
}

impl From<&ReleasePayload> for WireReleasePayload {
    fn from(value: &ReleasePayload) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            schema_version: 1,
            release_id: value.release_id.to_string(),
            body: WireReleaseBody::from(&value.body),
        }
    }
}

impl From<&ReleaseBody> for WireReleaseBody {
    fn from(value: &ReleaseBody) -> Self {
        Self {
            dataset_series_uuid: value.dataset_series_uuid.to_string(),
            release_ordinal: value.release_ordinal.to_string(),
            scientific_content_id: value.scientific_content_id.to_string(),
            package_id: value.package_id.to_string(),
            recipe_ids: value.recipe_ids.iter().map(ToString::to_string).collect(),
            derivation_record_ids: value
                .derivation_record_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            portable_record_digests: value
                .portable_record_digests
                .iter()
                .map(ToString::to_string)
                .collect(),
            schema_profiles: value
                .schema_profiles
                .iter()
                .map(ToString::to_string)
                .collect(),
            license_spdx: value.license_spdx.to_string(),
            rights_holders: value
                .rights_holders
                .iter()
                .map(ToString::to_string)
                .collect(),
            citation: WireReleaseCitation::from(&value.citation),
            creators: value.creators.iter().map(ToString::to_string).collect(),
            institutions: value.institutions.iter().map(ToString::to_string).collect(),
            funders: value.funders.iter().map(ToString::to_string).collect(),
            evidence: value
                .evidence
                .iter()
                .map(WireReleaseEvidence::from)
                .collect(),
            published_at: value.published_at.to_string(),
            supersedes: value.supersedes.iter().map(ToString::to_string).collect(),
        }
    }
}

impl From<&ReleaseCitation> for WireReleaseCitation {
    fn from(value: &ReleaseCitation) -> Self {
        Self {
            title: value.title.to_string(),
            year: value.year.map(|year| year.to_string()),
            doi: value.doi.as_ref().map(ToString::to_string),
        }
    }
}

impl From<&ReleaseEvidence> for WireReleaseEvidence {
    fn from(value: &ReleaseEvidence) -> Self {
        Self {
            kind: value.kind.to_string(),
            digest: value.digest.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(hex: char) -> ExactBytesDigest {
        ExactBytesDigest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn text(value: &str) -> NfcText {
        NfcText::parse(value).unwrap()
    }

    fn token(value: &str) -> AsciiToken {
        AsciiToken::parse(value).unwrap()
    }

    fn body() -> ReleaseBody {
        ReleaseBody::new(
            DatasetSeriesUuid::parse("123e4567-e89b-42d3-a456-426614174000").unwrap(),
            U64Decimal::parse("1").unwrap(),
            ScientificContentId::parse(&format!(
                "{}{}",
                ScientificContentId::PREFIX,
                "0".repeat(64)
            ))
            .unwrap(),
            PackageId::parse(&format!("{}{}", PackageId::PREFIX, "1".repeat(64))).unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                token("m4d-id-1"),
                token("m4d-science-1.0"),
                token("m4d-zarr3-local-1.0"),
            ],
            SpdxLicense::parse("CC-BY-4.0").unwrap(),
            vec![text("Alice")],
            ReleaseCitation::new(
                text("Dataset"),
                Some(U64Decimal::parse("2026").unwrap()),
                Some(Doi::parse("10.1234/example").unwrap()),
            )
            .unwrap(),
            vec![text("Alice"), text("Bob")],
            Vec::new(),
            Vec::new(),
            vec![ReleaseEvidence::new(token("checksum"), exact('2'))],
            PublishedAtUtc::parse("2026-07-12T12:34:56Z").unwrap(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn release_roundtrips_exact_body_and_verified_identity() {
        let body = body();
        let expected_body = r#"{"citation":{"doi":"10.1234/example","title":"Dataset","year":"2026"},"creators":["Alice","Bob"],"dataset_series_uuid":"123e4567-e89b-42d3-a456-426614174000","derivation_record_ids":[],"evidence":[{"digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","kind":"checksum"}],"funders":[],"institutions":[],"license_spdx":"CC-BY-4.0","package_id":"m4d-package-v1-sha256:1111111111111111111111111111111111111111111111111111111111111111","portable_record_digests":[],"published_at":"2026-07-12T12:34:56Z","recipe_ids":[],"release_ordinal":"1","rights_holders":["Alice"],"schema_profiles":["m4d-id-1","m4d-science-1.0","m4d-zarr3-local-1.0"],"scientific_content_id":"m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000","supersedes":[]}"#;
        assert_eq!(body.canonical_bytes().unwrap(), expected_body.as_bytes());

        let payload = ReleasePayload::new(body).unwrap();
        assert_eq!(expected_body.len(), 788);
        let expected_id = ReleaseId::parse(concat!(
            "m4d-release-v1-sha256:",
            "34880192ddf416c13484600f04504c03aafbe4e76d62a9a146d2565024b1731d"
        ))
        .unwrap();
        assert_eq!(payload.release_id(), expected_id);
        let expected = format!(
            r#"{{"body":{expected_body},"release_id":"{expected_id}","schema":"m4d-release","schema_version":1}}"#
        );
        let bytes = payload.canonical_bytes().unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(ReleasePayload::parse_canonical(&bytes).unwrap(), payload);
    }

    #[test]
    fn uuid_and_publication_time_use_one_strict_wire_form() {
        assert_eq!(
            DatasetSeriesUuid::parse("123e4567-e89b-42d3-a456-426614174000")
                .unwrap()
                .as_str(),
            "123e4567-e89b-42d3-a456-426614174000"
        );
        for value in [
            "123E4567-e89b-42d3-a456-426614174000",
            "123e4567-e89b-32d3-a456-426614174000",
            "123e4567-e89b-42d3-c456-426614174000",
            "123e4567e89b42d3a456426614174000",
        ] {
            assert!(DatasetSeriesUuid::parse(value).is_err(), "accepted {value}");
        }

        for value in [
            "0001-01-01T00:00:00Z",
            "2000-02-29T23:59:59Z",
            "9999-12-31T23:59:59Z",
        ] {
            assert_eq!(PublishedAtUtc::parse(value).unwrap().as_str(), value);
        }
        for value in [
            "0000-01-01T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:60Z",
            "2026-01-01T00:00:00+00:00",
        ] {
            assert!(PublishedAtUtc::parse(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn release_rejects_malformed_unordered_oversized_and_self_superseding_values() {
        let payload = ReleasePayload::new(body()).unwrap();
        let canonical = String::from_utf8(payload.canonical_bytes().unwrap()).unwrap();
        let wrong_id = canonical.replacen(
            &payload.release_id().to_string(),
            &format!("{}{}", ReleaseId::PREFIX, "f".repeat(64)),
            1,
        );
        for wire in [
            canonical.replacen("\"body\":", "\"body\":{},\"body\":", 1),
            canonical.replacen("\"schema\":", "\"extra\":false,\"schema\":", 1),
            format!(" {canonical}"),
            wrong_id,
        ] {
            assert!(
                ReleasePayload::parse_canonical(wire.as_bytes()).is_err(),
                "accepted {wire}"
            );
        }

        let mut unordered = body();
        unordered.schema_profiles.swap(0, 1);
        assert!(unordered.canonical_bytes().is_err());

        let mut self_superseding = payload;
        self_superseding.body.supersedes = vec![self_superseding.release_id];
        assert!(self_superseding.canonical_bytes().is_err());
        assert!(
            ReleasePayload::parse_canonical(&vec![b' '; MAX_PORTABLE_CONTROL_OBJECT_BYTES + 1])
                .is_err()
        );
    }
}
