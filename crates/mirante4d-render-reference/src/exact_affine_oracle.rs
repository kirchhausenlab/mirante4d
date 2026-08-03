//! Independent exact-dyadic verification for admitted shader affines.
//!
//! Binary32 values are integer multiples of `2^-149`. Scaling the uploaded
//! world-to-grid matrix by `2^149` therefore produces an exact integer matrix.
//! This module forms its adjugate and determinant with a fixed stack integer;
//! it does not call the production LU/residual implementation.

use std::cmp::Ordering;

use mirante4d_render_api::ValidatedShaderAffine;

// A binary32 coefficient scaled by 2^149 occupies at most 254 bits. A 3x3
// determinant occupies fewer than 766 bits. The f64 interval comparison can
// additionally shift by 1,074 bits, so 2,560 bits leaves a conservative fixed
// margin while retaining stack-only, dependency-free arithmetic.
const LIMBS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SignedInt {
    negative: bool,
    limbs: [u64; LIMBS],
}

impl SignedInt {
    const ZERO: Self = Self {
        negative: false,
        limbs: [0; LIMBS],
    };

    fn from_u64(value: u64) -> Self {
        let mut result = Self::ZERO;
        result.limbs[0] = value;
        result
    }

    fn is_zero(self) -> bool {
        self.limbs.iter().all(|limb| *limb == 0)
    }

    fn negated(mut self) -> Self {
        if !self.is_zero() {
            self.negative = !self.negative;
        }
        self
    }

    fn magnitude_cmp(self, other: Self) -> Ordering {
        self.limbs
            .iter()
            .rev()
            .zip(other.limbs.iter().rev())
            .find_map(|(left, right)| (left != right).then(|| left.cmp(right)))
            .unwrap_or(Ordering::Equal)
    }

    fn magnitude_add(self, other: Self) -> Self {
        let mut result = Self::ZERO;
        let mut carry = 0_u128;
        for index in 0..LIMBS {
            let total = u128::from(self.limbs[index]) + u128::from(other.limbs[index]) + carry;
            result.limbs[index] = total as u64;
            carry = total >> 64;
        }
        assert_eq!(
            carry, 0,
            "the exact-affine oracle integer width is sufficient"
        );
        result
    }

    fn magnitude_sub(self, other: Self) -> Self {
        debug_assert!(self.magnitude_cmp(other) != Ordering::Less);
        let mut result = Self::ZERO;
        let mut borrow = 0_u128;
        for index in 0..LIMBS {
            let left = u128::from(self.limbs[index]);
            let right = u128::from(other.limbs[index]) + borrow;
            if left >= right {
                result.limbs[index] = (left - right) as u64;
                borrow = 0;
            } else {
                result.limbs[index] = ((1_u128 << 64) + left - right) as u64;
                borrow = 1;
            }
        }
        debug_assert_eq!(borrow, 0);
        result
    }

    fn add(self, other: Self) -> Self {
        if self.negative == other.negative {
            let mut result = self.magnitude_add(other);
            result.negative = self.negative && !result.is_zero();
            return result;
        }
        match self.magnitude_cmp(other) {
            Ordering::Greater => {
                let mut result = self.magnitude_sub(other);
                result.negative = self.negative && !result.is_zero();
                result
            }
            Ordering::Less => {
                let mut result = other.magnitude_sub(self);
                result.negative = other.negative && !result.is_zero();
                result
            }
            Ordering::Equal => Self::ZERO,
        }
    }

    fn sub(self, other: Self) -> Self {
        self.add(other.negated())
    }

    fn mul(self, other: Self) -> Self {
        let mut result = Self::ZERO;
        for left_index in 0..LIMBS {
            if self.limbs[left_index] == 0 {
                continue;
            }
            let mut carry = 0_u128;
            for right_index in 0..(LIMBS - left_index) {
                if other.limbs[right_index] == 0 && carry == 0 {
                    continue;
                }
                let target = left_index + right_index;
                let total = u128::from(self.limbs[left_index])
                    * u128::from(other.limbs[right_index])
                    + u128::from(result.limbs[target])
                    + carry;
                result.limbs[target] = total as u64;
                carry = total >> 64;
            }
            assert_eq!(
                carry, 0,
                "the exact-affine oracle integer width is sufficient"
            );
        }
        result.negative = self.negative != other.negative && !result.is_zero();
        result
    }

    fn shifted_left(self, bits: usize) -> Self {
        if self.is_zero() || bits == 0 {
            return self;
        }
        let word_shift = bits / 64;
        let bit_shift = bits % 64;
        assert!(
            word_shift < LIMBS,
            "the exact-affine shift fits its fixed width"
        );
        assert!(
            self.limbs[(LIMBS - word_shift)..]
                .iter()
                .all(|limb| *limb == 0),
            "the exact-affine shift fits its fixed width"
        );
        let mut result = Self::ZERO;
        result.negative = self.negative;
        for source in 0..(LIMBS - word_shift) {
            let target = source + word_shift;
            result.limbs[target] |= self.limbs[source] << bit_shift;
            if bit_shift != 0 && target + 1 < LIMBS {
                result.limbs[target + 1] |= self.limbs[source] >> (64 - bit_shift);
            } else if bit_shift != 0 {
                assert_eq!(self.limbs[source] >> (64 - bit_shift), 0);
            }
        }
        result
    }

    fn signed_cmp(self, other: Self) -> Ordering {
        match (self.negative, other.negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self.magnitude_cmp(other),
            (true, true) => other.magnitude_cmp(self),
        }
    }
}

fn scaled_binary32_integer(value: f32) -> Option<SignedInt> {
    let bits = value.to_bits();
    let exponent = (bits >> 23) & 0xff;
    if exponent == 0xff {
        return None;
    }
    let fraction = u64::from(bits & 0x7f_ff_ff);
    let (significand, shift) = if exponent == 0 {
        (fraction, 0_usize)
    } else {
        (
            (1_u64 << 23) | fraction,
            usize::try_from(exponent - 1).expect("a binary32 exponent fits usize"),
        )
    };
    let mut result = SignedInt::from_u64(significand).shifted_left(shift);
    result.negative = bits >> 31 != 0 && !result.is_zero();
    Some(result)
}

fn decoded_binary64(value: f64) -> Option<(SignedInt, i32)> {
    if !value.is_finite() {
        return None;
    }
    let bits = value.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (significand, exponent) = if exponent_bits == 0 {
        (fraction, -1074)
    } else {
        ((1_u64 << 52) | fraction, exponent_bits - 1023 - 52)
    };
    let mut integer = SignedInt::from_u64(significand);
    integer.negative = bits >> 63 != 0 && !integer.is_zero();
    Some((integer, exponent))
}

fn rational_cmp_f64(
    mut numerator: SignedInt,
    mut denominator: SignedInt,
    value: f64,
) -> Option<Ordering> {
    if denominator.is_zero() {
        return None;
    }
    if denominator.negative {
        denominator = denominator.negated();
        numerator = numerator.negated();
    }
    if value == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if value == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    let (value_integer, exponent) = decoded_binary64(value)?;
    let right = denominator.mul(value_integer);
    let (left, right) = if exponent >= 0 {
        (numerator, right.shifted_left(exponent as usize))
    } else {
        (numerator.shifted_left((-exponent) as usize), right)
    };
    Some(left.signed_cmp(right))
}

fn next_down(value: f64) -> f64 {
    if value.is_nan() || value == f64::NEG_INFINITY {
        value
    } else if value == 0.0 {
        -f64::from_bits(1)
    } else if value > 0.0 {
        f64::from_bits(value.to_bits() - 1)
    } else {
        f64::from_bits(value.to_bits() + 1)
    }
}

fn next_up(value: f64) -> f64 {
    if value.is_nan() || value == f64::INFINITY {
        value
    } else if value == 0.0 {
        f64::from_bits(1)
    } else if value > 0.0 {
        f64::from_bits(value.to_bits() + 1)
    } else {
        f64::from_bits(value.to_bits() - 1)
    }
}

fn two_by_two(a: SignedInt, b: SignedInt, c: SignedInt, d: SignedInt) -> SignedInt {
    a.mul(d).sub(b.mul(c))
}

fn exact_inverse_numerators(rows: [[f32; 4]; 3]) -> Option<(SignedInt, [[SignedInt; 4]; 3])> {
    if rows.iter().flatten().any(|value| !value.is_finite()) {
        return None;
    }
    let matrix = rows.map(|row| {
        row.map(scaled_binary32_integer)
            .map(|value| value.expect("finite rows were checked before exact inversion"))
    });
    let [a, b, c, tx] = matrix[0];
    let [d, e, f, ty] = matrix[1];
    let [g, h, i, tz] = matrix[2];
    let adjugate = [
        [
            two_by_two(e, f, h, i),
            two_by_two(c, b, i, h),
            two_by_two(b, c, e, f),
        ],
        [
            two_by_two(f, d, i, g),
            two_by_two(a, c, g, i),
            two_by_two(c, a, f, d),
        ],
        [
            two_by_two(d, e, g, h),
            two_by_two(b, a, h, g),
            two_by_two(a, b, d, e),
        ],
    ];
    let determinant = a
        .mul(adjugate[0][0])
        .add(b.mul(adjugate[1][0]))
        .add(c.mul(adjugate[2][0]));
    if determinant.is_zero() {
        return None;
    }
    let translation = [tx, ty, tz];
    let numerators = std::array::from_fn(|row| {
        let linear: [SignedInt; 3] =
            std::array::from_fn(|column| adjugate[row][column].shifted_left(149));
        let translated = (0..3)
            .map(|column| adjugate[row][column].mul(translation[column]))
            .fold(SignedInt::ZERO, SignedInt::add)
            .negated();
        [linear[0], linear[1], linear[2], translated]
    });
    Some((determinant, numerators))
}

/// Returns true only when every exact inverse of the uploaded binary32
/// world-to-grid affine lies inside the production center/radius interval.
pub fn quantized_affine_inverse_is_contained(affine: &ValidatedShaderAffine) -> bool {
    let Some((denominator, numerators)) = exact_inverse_numerators(affine.world_to_grid_rows())
    else {
        return false;
    };
    let centers = affine.quantized_inverse_center();
    let radii = affine.quantized_inverse_radius();
    (0..3).all(|row| {
        (0..4).all(|column| {
            let center = centers[row][column];
            let radius = radii[row][column];
            if !center.is_finite() || !radius.is_finite() || radius < 0.0 {
                return false;
            }
            let lower = next_down(center - radius);
            let upper = next_up(center + radius);
            matches!(
                rational_cmp_f64(numerators[row][column], denominator, lower),
                Some(Ordering::Greater | Ordering::Equal)
            ) && matches!(
                rational_cmp_f64(numerators[row][column], denominator, upper),
                Some(Ordering::Less | Ordering::Equal)
            )
        })
    })
}

/// Independently evaluates every declared half-voxel grid corner through the
/// durable affine and the uploaded binary32 inverse. Each arithmetic step is
/// rounded explicitly to binary32, without using the production error-bound
/// implementation.
pub fn quantized_affine_grid_corners_are_bounded(affine: &ValidatedShaderAffine) -> bool {
    let shape = affine.grid_shape();
    let bounds = [
        [-0.5, shape.x() as f64 - 0.5],
        [-0.5, shape.y() as f64 - 0.5],
        [-0.5, shape.z() as f64 - 0.5],
    ];
    let transform = affine.grid_to_world().row_major();
    let inverse = affine.world_to_grid_rows();
    let declared = affine.maximum_grid_error();
    (0_u8..8).all(|mask| {
        let grid = std::array::from_fn::<_, 3, _>(|axis| {
            bounds[axis][usize::from(mask & (1 << axis) != 0)]
        });
        let world = [
            transform[0] * grid[0] + transform[1] * grid[1] + transform[2] * grid[2] + transform[3],
            transform[4] * grid[0] + transform[5] * grid[1] + transform[6] * grid[2] + transform[7],
            transform[8] * grid[0]
                + transform[9] * grid[1]
                + transform[10] * grid[2]
                + transform[11],
        ]
        .map(|value| value as f32);
        (0..3).all(|axis| {
            let row = inverse[axis];
            let products = [
                (f64::from(row[0]) * f64::from(world[0])) as f32,
                (f64::from(row[1]) * f64::from(world[1])) as f32,
                (f64::from(row[2]) * f64::from(world[2])) as f32,
            ];
            let sum01 = (f64::from(products[0]) + f64::from(products[1])) as f32;
            let sum012 = (f64::from(sum01) + f64::from(products[2])) as f32;
            let reconstructed = (f64::from(sum012) + f64::from(row[3])) as f32;
            let error = (f64::from(reconstructed) - grid[axis]).abs();
            error < declared[axis]
        })
    })
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::{GridToWorld, Shape3D};
    use mirante4d_render_api::ValidatedShaderAffine;

    use super::{quantized_affine_grid_corners_are_bounded, quantized_affine_inverse_is_contained};

    #[test]
    fn verified_quantized_inverse_radius_contains_exact_reference_inverse() {
        let shape = Shape3D::new(31, 23, 17).unwrap();
        let transforms = [
            GridToWorld::identity(),
            GridToWorld::scale(1.0e-6, 1.0e-6, 1.0e-6).unwrap(),
            GridToWorld::scale(2.0e6, 2.0e6, 2.0e6).unwrap(),
            GridToWorld::from_row_major([
                1.25, 0.125, -0.03125, 7.5, -0.25, 2.0, 0.0625, -11.0, 0.015625, -0.125, 0.75,
                3.25, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            GridToWorld::from_row_major([
                1.0,
                0.999_999_940_395_355_2,
                0.0,
                1.0,
                0.0,
                1.0,
                0.125,
                32_768.0,
                0.0625,
                0.0,
                1.0,
                -4_096.0,
                0.0,
                0.0,
                0.0,
                1.0,
            ])
            .unwrap(),
        ];
        for transform in transforms {
            let affine = ValidatedShaderAffine::new(transform, shape)
                .expect("the independent exact-inverse fixture is render-admissible");
            assert!(
                quantized_affine_inverse_is_contained(&affine),
                "the exact inverse must be enclosed for {transform:?}"
            );
        }
    }

    #[test]
    fn quantized_affine_error_envelope_bounds_every_grid_corner() {
        let shape = Shape3D::new(31, 23, 17).unwrap();
        let affine = ValidatedShaderAffine::new(
            GridToWorld::from_row_major([
                1.25, 0.125, -0.03125, 7.5, -0.25, 2.0, 0.0625, -11.0, 0.015625, -0.125, 0.75,
                3.25, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            shape,
        )
        .expect("the independent corner fixture is render-admissible");
        assert!(quantized_affine_grid_corners_are_bounded(&affine));

        let outside = GridToWorld::from_row_major([
            1.0,
            0.0,
            0.0,
            16_777_216.75,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .unwrap();
        assert!(ValidatedShaderAffine::new(outside, shape).is_err());
    }
}
