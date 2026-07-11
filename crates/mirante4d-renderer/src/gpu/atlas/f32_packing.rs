use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, ResourcePayloadView, ResourceValidity,
};
use mirante4d_domain::IntensityDType;

use crate::{RenderError, gpu::GpuRenderError};

pub(super) enum F32UploadBytes<'a> {
    Borrowed(&'a [u8]),
    Staged {
        bytes: Vec<u8>,
        _charge: Box<dyn CpuByteLease>,
    },
}

impl<'a> F32UploadBytes<'a> {
    pub(super) fn new(
        payload: ResourcePayloadView<'a>,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<Self, GpuRenderError> {
        if payload.dtype() != IntensityDType::Float32 {
            return Err(RenderError::InvalidBrickAtlas(
                "float32 atlas received a non-float32 lease payload",
            )
            .into());
        }
        if payload.validity() == ResourceValidity::AllValid {
            return Ok(Self::Borrowed(payload.value_bytes()));
        }

        let charge =
            cpu_ledger.try_acquire(CpuLedgerCategory::UploadStaging, payload.value_byte_len())?;
        let mut bytes = payload.value_bytes().to_vec();
        for sample in 0..payload.sample_count() {
            if payload.sample_is_valid(sample).map_err(RenderError::from)? {
                continue;
            }
            let offset = usize::try_from(sample.checked_mul(4).ok_or(
                RenderError::InvalidBrickAtlas("float32 staging offset overflows"),
            )?)
            .map_err(|_| RenderError::InvalidBrickAtlas("float32 offset exceeds usize"))?;
            bytes[offset..offset + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        }
        Ok(Self::Staged {
            bytes,
            _charge: charge,
        })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Staged { bytes, .. } => bytes,
        }
    }
}
