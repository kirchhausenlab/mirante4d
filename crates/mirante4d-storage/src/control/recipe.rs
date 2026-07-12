use mirante4d_identity::{ExactBytesDigest, RecipeId};
use serde::{Deserialize, Serialize};

use super::{
    AsciiToken, CanonicalValue, ControlError, MAX_PORTABLE_CONTROL_OBJECT_BYTES, U64Decimal, jcs,
    value::WireValue,
};

const OBJECT: &str = "recipe payload";

/// The closed reproducibility class for a version-1 recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipeDeterminism {
    BitExact,
    NumericallyBounded,
    NonDeterministic,
}

impl RecipeDeterminism {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BitExact => "bit_exact",
            Self::NumericallyBounded => "numerically_bounded",
            Self::NonDeterministic => "non_deterministic",
        }
    }
}

/// An explicit deterministic random-number policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeRng {
    algorithm: AsciiToken,
    seed: U64Decimal,
}

impl RecipeRng {
    pub const fn new(algorithm: AsciiToken, seed: U64Decimal) -> Self {
        Self { algorithm, seed }
    }

    pub const fn algorithm(&self) -> &AsciiToken {
        &self.algorithm
    }

    pub const fn seed(&self) -> U64Decimal {
        self.seed
    }
}

/// The exact numeric-execution policy attached to one recipe operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeNumericPolicy {
    dtype: AsciiToken,
    rounding: AsciiToken,
    reduction: AsciiToken,
    kernel: AsciiToken,
    boundary: AsciiToken,
    interpolation: AsciiToken,
    no_data: AsciiToken,
    ordering: AsciiToken,
    precision: AsciiToken,
    rng: Option<RecipeRng>,
}

impl RecipeNumericPolicy {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        dtype: AsciiToken,
        rounding: AsciiToken,
        reduction: AsciiToken,
        kernel: AsciiToken,
        boundary: AsciiToken,
        interpolation: AsciiToken,
        no_data: AsciiToken,
        ordering: AsciiToken,
        precision: AsciiToken,
        rng: Option<RecipeRng>,
    ) -> Self {
        Self {
            dtype,
            rounding,
            reduction,
            kernel,
            boundary,
            interpolation,
            no_data,
            ordering,
            precision,
            rng,
        }
    }

    pub const fn rng(&self) -> Option<&RecipeRng> {
        self.rng.as_ref()
    }

    pub const fn dtype(&self) -> &AsciiToken {
        &self.dtype
    }

    pub const fn rounding(&self) -> &AsciiToken {
        &self.rounding
    }

    pub const fn reduction(&self) -> &AsciiToken {
        &self.reduction
    }

    pub const fn kernel(&self) -> &AsciiToken {
        &self.kernel
    }

    pub const fn boundary(&self) -> &AsciiToken {
        &self.boundary
    }

    pub const fn interpolation(&self) -> &AsciiToken {
        &self.interpolation
    }

    pub const fn no_data(&self) -> &AsciiToken {
        &self.no_data
    }

    pub const fn ordering(&self) -> &AsciiToken {
        &self.ordering
    }

    pub const fn precision(&self) -> &AsciiToken {
        &self.precision
    }
}

/// One role-labelled edge from an earlier recipe node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeInput {
    node: U64Decimal,
    role: AsciiToken,
}

impl RecipeInput {
    pub const fn new(node: U64Decimal, role: AsciiToken) -> Self {
        Self { node, role }
    }

    pub const fn node(&self) -> U64Decimal {
        self.node
    }

    pub const fn role(&self) -> &AsciiToken {
        &self.role
    }
}

/// One validated node in a version-1 recipe graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeOperation {
    node: U64Decimal,
    name: AsciiToken,
    semantic_version: AsciiToken,
    parameter_schema: AsciiToken,
    parameters: CanonicalValue,
    inputs: Vec<RecipeInput>,
    numeric_policy: RecipeNumericPolicy,
    output_roles: Vec<AsciiToken>,
}

impl RecipeOperation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node: U64Decimal,
        name: AsciiToken,
        semantic_version: AsciiToken,
        parameter_schema: AsciiToken,
        parameters: CanonicalValue,
        inputs: Vec<RecipeInput>,
        numeric_policy: RecipeNumericPolicy,
        output_roles: Vec<AsciiToken>,
    ) -> Result<Self, ControlError> {
        if !inputs
            .windows(2)
            .all(|pair| (&pair[0].role, pair[0].node) < (&pair[1].role, pair[1].node))
        {
            return invalid(
                "operation inputs must be strictly sorted and unique by role then node",
            );
        }
        if inputs.iter().any(|input| input.node.get() >= node.get()) {
            return invalid("operation inputs must reference earlier nodes");
        }
        if output_roles.is_empty() || !output_roles.windows(2).all(|pair| pair[0] < pair[1]) {
            return invalid("output roles must be nonempty, strictly sorted, and unique");
        }
        Ok(Self {
            node,
            name,
            semantic_version,
            parameter_schema,
            parameters,
            inputs,
            numeric_policy,
            output_roles,
        })
    }

    pub const fn node(&self) -> U64Decimal {
        self.node
    }

    pub const fn name(&self) -> &AsciiToken {
        &self.name
    }

    pub const fn semantic_version(&self) -> &AsciiToken {
        &self.semantic_version
    }

    pub const fn parameter_schema(&self) -> &AsciiToken {
        &self.parameter_schema
    }

    pub const fn parameters(&self) -> &CanonicalValue {
        &self.parameters
    }

    pub fn inputs(&self) -> &[RecipeInput] {
        &self.inputs
    }

    pub fn output_roles(&self) -> &[AsciiToken] {
        &self.output_roles
    }

    pub const fn numeric_policy(&self) -> &RecipeNumericPolicy {
        &self.numeric_policy
    }
}

/// The identity-bearing canonical body of one version-1 recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeBody {
    operation_registry_digest: ExactBytesDigest,
    determinism: RecipeDeterminism,
    operations: Vec<RecipeOperation>,
}

impl RecipeBody {
    pub fn new(
        operation_registry_digest: ExactBytesDigest,
        determinism: RecipeDeterminism,
        operations: Vec<RecipeOperation>,
    ) -> Result<Self, ControlError> {
        require_operation_order(&operations)?;
        Ok(Self {
            operation_registry_digest,
            determinism,
            operations,
        })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        require_operation_order(&self.operations)?;
        let wire = WireRecipeBody::from(self);
        let value =
            serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            })?;
        jcs::encode(&value, "recipe body", MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub const fn operation_registry_digest(&self) -> ExactBytesDigest {
        self.operation_registry_digest
    }

    pub const fn determinism(&self) -> RecipeDeterminism {
        self.determinism
    }

    pub fn operations(&self) -> &[RecipeOperation] {
        &self.operations
    }
}

/// A canonical recipe body bound to its verified version-1 RecipeId.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipePayload {
    recipe_id: RecipeId,
    body: RecipeBody,
}

impl RecipePayload {
    pub fn new(body: RecipeBody) -> Result<Self, ControlError> {
        let bytes = body.canonical_bytes()?;
        let recipe_id = RecipeId::from_canonical_body_bytes(&bytes).map_err(|_| {
            ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "recipe body length exceeds identity framing",
            }
        })?;
        Ok(Self { recipe_id, body })
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireRecipePayload = serde_json::from_slice(bytes).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        let recipe_id =
            RecipeId::parse(&wire.recipe_id).map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "recipe_id is invalid",
            })?;
        let body = RecipeBody::try_from(wire.body)?;
        let value = Self::new(body)?;
        if value.recipe_id != recipe_id {
            return invalid("recipe_id does not verify the canonical body");
        }
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject { object: OBJECT });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        let body_bytes = self.body.canonical_bytes()?;
        if !self
            .recipe_id
            .matches_canonical_body_bytes(&body_bytes)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "recipe body length exceeds identity framing",
            })?
        {
            return invalid("recipe_id does not verify the canonical body");
        }
        let wire = WireRecipePayload::from(self);
        let value =
            serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            })?;
        jcs::encode(&value, OBJECT, MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub const fn recipe_id(&self) -> RecipeId {
        self.recipe_id
    }

    pub const fn body(&self) -> &RecipeBody {
        &self.body
    }
}

fn require_operation_order(operations: &[RecipeOperation]) -> Result<(), ControlError> {
    if operations.is_empty() {
        return invalid("recipe operations must be nonempty");
    }
    for (expected, operation) in operations.iter().enumerate() {
        if u64::try_from(expected).ok() != Some(operation.node.get()) {
            return invalid("recipe nodes must be contiguous and zero-based");
        }
    }
    Ok(())
}

fn invalid<T>(reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidControlObject {
        object: OBJECT,
        reason,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecipePayload {
    recipe_id: String,
    body: WireRecipeBody,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecipeBody {
    operation_registry_digest: String,
    determinism: String,
    operations: Vec<WireRecipeOperation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecipeOperation {
    node: String,
    name: String,
    semantic_version: String,
    parameter_schema: String,
    parameters: WireValue,
    inputs: Vec<WireRecipeInput>,
    numeric_policy: WireRecipeNumericPolicy,
    output_roles: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecipeInput {
    node: String,
    role: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecipeNumericPolicy {
    dtype: String,
    rounding: String,
    reduction: String,
    kernel: String,
    boundary: String,
    interpolation: String,
    no_data: String,
    ordering: String,
    precision: String,
    rng: Option<WireRecipeRng>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecipeRng {
    algorithm: String,
    seed: String,
}

impl TryFrom<WireRecipeBody> for RecipeBody {
    type Error = ControlError;

    fn try_from(wire: WireRecipeBody) -> Result<Self, Self::Error> {
        let operation_registry_digest = ExactBytesDigest::parse(&wire.operation_registry_digest)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "operation_registry_digest is invalid",
            })?;
        let determinism = match wire.determinism.as_str() {
            "bit_exact" => RecipeDeterminism::BitExact,
            "numerically_bounded" => RecipeDeterminism::NumericallyBounded,
            "non_deterministic" => RecipeDeterminism::NonDeterministic,
            _ => return invalid("recipe determinism is not admitted"),
        };
        let operations = wire
            .operations
            .into_iter()
            .map(RecipeOperation::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(operation_registry_digest, determinism, operations)
    }
}

impl TryFrom<WireRecipeOperation> for RecipeOperation {
    type Error = ControlError;

    fn try_from(wire: WireRecipeOperation) -> Result<Self, Self::Error> {
        Self::new(
            U64Decimal::parse(&wire.node)?,
            AsciiToken::parse(&wire.name)?,
            AsciiToken::parse(&wire.semantic_version)?,
            AsciiToken::parse(&wire.parameter_schema)?,
            CanonicalValue::try_from(wire.parameters)?,
            wire.inputs
                .into_iter()
                .map(RecipeInput::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            RecipeNumericPolicy::try_from(wire.numeric_policy)?,
            wire.output_roles
                .into_iter()
                .map(|role| AsciiToken::parse(&role))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl TryFrom<WireRecipeInput> for RecipeInput {
    type Error = ControlError;

    fn try_from(wire: WireRecipeInput) -> Result<Self, Self::Error> {
        Ok(Self::new(
            U64Decimal::parse(&wire.node)?,
            AsciiToken::parse(&wire.role)?,
        ))
    }
}

impl TryFrom<WireRecipeNumericPolicy> for RecipeNumericPolicy {
    type Error = ControlError;

    fn try_from(wire: WireRecipeNumericPolicy) -> Result<Self, Self::Error> {
        Ok(Self::new(
            AsciiToken::parse(&wire.dtype)?,
            AsciiToken::parse(&wire.rounding)?,
            AsciiToken::parse(&wire.reduction)?,
            AsciiToken::parse(&wire.kernel)?,
            AsciiToken::parse(&wire.boundary)?,
            AsciiToken::parse(&wire.interpolation)?,
            AsciiToken::parse(&wire.no_data)?,
            AsciiToken::parse(&wire.ordering)?,
            AsciiToken::parse(&wire.precision)?,
            wire.rng.map(RecipeRng::try_from).transpose()?,
        ))
    }
}

impl TryFrom<WireRecipeRng> for RecipeRng {
    type Error = ControlError;

    fn try_from(wire: WireRecipeRng) -> Result<Self, Self::Error> {
        Ok(Self::new(
            AsciiToken::parse(&wire.algorithm)?,
            U64Decimal::parse(&wire.seed)?,
        ))
    }
}

impl From<&RecipePayload> for WireRecipePayload {
    fn from(value: &RecipePayload) -> Self {
        Self {
            recipe_id: value.recipe_id.to_string(),
            body: WireRecipeBody::from(&value.body),
        }
    }
}

impl From<&RecipeBody> for WireRecipeBody {
    fn from(value: &RecipeBody) -> Self {
        Self {
            operation_registry_digest: value.operation_registry_digest.to_string(),
            determinism: value.determinism.as_str().to_owned(),
            operations: value
                .operations
                .iter()
                .map(WireRecipeOperation::from)
                .collect(),
        }
    }
}

impl From<&RecipeOperation> for WireRecipeOperation {
    fn from(value: &RecipeOperation) -> Self {
        Self {
            node: value.node.to_string(),
            name: value.name.to_string(),
            semantic_version: value.semantic_version.to_string(),
            parameter_schema: value.parameter_schema.to_string(),
            parameters: WireValue::from(&value.parameters),
            inputs: value.inputs.iter().map(WireRecipeInput::from).collect(),
            numeric_policy: WireRecipeNumericPolicy::from(&value.numeric_policy),
            output_roles: value.output_roles.iter().map(ToString::to_string).collect(),
        }
    }
}

impl From<&RecipeInput> for WireRecipeInput {
    fn from(value: &RecipeInput) -> Self {
        Self {
            node: value.node.to_string(),
            role: value.role.to_string(),
        }
    }
}

impl From<&RecipeNumericPolicy> for WireRecipeNumericPolicy {
    fn from(value: &RecipeNumericPolicy) -> Self {
        Self {
            dtype: value.dtype.to_string(),
            rounding: value.rounding.to_string(),
            reduction: value.reduction.to_string(),
            kernel: value.kernel.to_string(),
            boundary: value.boundary.to_string(),
            interpolation: value.interpolation.to_string(),
            no_data: value.no_data.to_string(),
            ordering: value.ordering.to_string(),
            precision: value.precision.to_string(),
            rng: value.rng.as_ref().map(WireRecipeRng::from),
        }
    }
}

impl From<&RecipeRng> for WireRecipeRng {
    fn from(value: &RecipeRng) -> Self {
        Self {
            algorithm: value.algorithm.to_string(),
            seed: value.seed.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> AsciiToken {
        AsciiToken::parse(value).unwrap()
    }

    fn number(value: &str) -> U64Decimal {
        U64Decimal::parse(value).unwrap()
    }

    fn policy() -> RecipeNumericPolicy {
        RecipeNumericPolicy::new(
            token("uint16"),
            token("nearest"),
            token("pairwise"),
            token("mean"),
            token("reflect"),
            token("linear"),
            token("ignore"),
            token("tzyx"),
            token("exact"),
            Some(RecipeRng::new(token("pcg64"), number("7"))),
        )
    }

    fn operation(node: &str, inputs: Vec<RecipeInput>) -> RecipeOperation {
        RecipeOperation::new(
            number(node),
            token("downsample"),
            token("1.0.0"),
            token("m4d.params.v1"),
            CanonicalValue::map(Vec::new()).unwrap(),
            inputs,
            policy(),
            vec![token("image")],
        )
        .unwrap()
    }

    fn payload() -> RecipePayload {
        let digest = ExactBytesDigest::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
        RecipePayload::new(
            RecipeBody::new(
                digest,
                RecipeDeterminism::BitExact,
                vec![operation("0", Vec::new())],
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn recipe_payload_roundtrips_exact_body_and_verified_identity() {
        let value = payload();
        let expected_body = r#"{"determinism":"bit_exact","operation_registry_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","operations":[{"inputs":[],"name":"downsample","node":"0","numeric_policy":{"boundary":"reflect","dtype":"uint16","interpolation":"linear","kernel":"mean","no_data":"ignore","ordering":"tzyx","precision":"exact","reduction":"pairwise","rng":{"algorithm":"pcg64","seed":"7"},"rounding":"nearest"},"output_roles":["image"],"parameter_schema":"m4d.params.v1","parameters":{"entries":[],"type":"map"},"semantic_version":"1.0.0"}]}"#;
        assert_eq!(
            value.body().canonical_bytes().unwrap(),
            expected_body.as_bytes()
        );
        let expected_id = RecipeId::from_canonical_body_bytes(expected_body.as_bytes()).unwrap();
        assert_eq!(value.recipe_id(), expected_id);
        let expected = format!(r#"{{"body":{expected_body},"recipe_id":"{expected_id}"}}"#);
        let bytes = value.canonical_bytes().unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(RecipePayload::parse_canonical(&bytes).unwrap(), value);
    }

    #[test]
    fn recipe_payload_rejects_malformed_noncanonical_and_invalid_graphs() {
        let valid = payload();
        let canonical = String::from_utf8(valid.canonical_bytes().unwrap()).unwrap();
        let wrong_id = canonical.replacen(
            &valid.recipe_id().to_string(),
            &format!("{}{}", RecipeId::PREFIX, "0".repeat(64)),
            1,
        );
        for wire in [
            canonical.replacen("\"body\":", "\"body\":{},\"body\":", 1),
            canonical.replacen("\"operations\":", "\"extra\":false,\"operations\":", 1),
            format!(" {canonical}"),
            wrong_id,
            canonical.replacen("\"node\":\"0\"", "\"node\":\"00\"", 1),
        ] {
            assert!(
                RecipePayload::parse_canonical(wire.as_bytes()).is_err(),
                "accepted {wire}"
            );
        }

        let digest = ExactBytesDigest::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(RecipeBody::new(digest, RecipeDeterminism::BitExact, Vec::new()).is_err());
        assert!(
            RecipeBody::new(
                digest,
                RecipeDeterminism::BitExact,
                vec![operation("1", Vec::new())]
            )
            .is_err()
        );
        assert!(
            RecipeOperation::new(
                number("1"),
                token("op"),
                token("1"),
                token("params"),
                CanonicalValue::from_bool(true),
                vec![
                    RecipeInput::new(number("0"), token("z")),
                    RecipeInput::new(number("0"), token("a")),
                ],
                policy(),
                vec![token("out")],
            )
            .is_err()
        );
        let mut no_rng = payload();
        no_rng.body.operations[0].numeric_policy.rng = None;
        no_rng = RecipePayload::new(no_rng.body).unwrap();
        let no_rng_bytes = no_rng.canonical_bytes().unwrap();
        assert!(String::from_utf8_lossy(&no_rng_bytes).contains("\"rng\":null"));
        assert_eq!(
            RecipePayload::parse_canonical(&no_rng_bytes).unwrap(),
            no_rng
        );
        assert!(
            RecipeOperation::new(
                number("0"),
                token("op"),
                token("1"),
                token("params"),
                CanonicalValue::from_bool(true),
                vec![RecipeInput::new(number("0"), token("self"))],
                policy(),
                vec![token("out")],
            )
            .is_err()
        );
        assert!(
            RecipePayload::parse_canonical(&vec![b' '; MAX_PORTABLE_CONTROL_OBJECT_BYTES + 1])
                .is_err()
        );
    }
}
