use thiserror::Error;

pub const AXES_TZYX: [&str; 4] = ["t", "z", "y", "x"];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AxisError {
    #[error("axis order must be exactly [\"t\", \"z\", \"y\", \"x\"], got {got:?}")]
    InvalidOrder { got: Vec<String> },
}

pub fn validate_axes_tzyx<S>(axes: &[S]) -> Result<(), AxisError>
where
    S: AsRef<str>,
{
    if axes.len() == AXES_TZYX.len()
        && axes
            .iter()
            .zip(AXES_TZYX)
            .all(|(actual, expected)| actual.as_ref() == expected)
    {
        Ok(())
    } else {
        Err(AxisError::InvalidOrder {
            got: axes.iter().map(|axis| axis.as_ref().to_owned()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bootstrap_axis_order() {
        validate_axes_tzyx(&["t", "z", "y", "x"]).unwrap();
    }

    #[test]
    fn rejects_channel_axis() {
        let err = validate_axes_tzyx(&["t", "z", "y", "x", "c"]).unwrap_err();
        assert_eq!(
            err,
            AxisError::InvalidOrder {
                got: vec!["t", "z", "y", "x", "c"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            }
        );
    }
}
