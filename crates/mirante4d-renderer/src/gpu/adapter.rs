use super::GpuRenderError;

pub const REQUIRED_MAX_BUFFER_SIZE: u64 = 256 * 1024 * 1024;
pub const REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE: u64 = 256 * 1024 * 1024;
pub const REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuLimitDiagnostics {
    pub max_buffer_size: u64,
    pub max_storage_buffer_binding_size: u64,
    pub max_storage_buffers_per_shader_stage: u32,
}

impl From<&wgpu::Limits> for GpuLimitDiagnostics {
    fn from(limits: &wgpu::Limits) -> Self {
        Self {
            max_buffer_size: limits.max_buffer_size,
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            max_storage_buffers_per_shader_stage: limits.max_storage_buffers_per_shader_stage,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterDiagnostics {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub timestamp_queries_supported: bool,
    pub timestamp_queries_requested: bool,
    pub timestamp_queries_enabled: bool,
    pub adapter_limits: GpuLimitDiagnostics,
    pub requested_limits: GpuLimitDiagnostics,
}

pub const GPU_ADAPTER_ENV: &str = "MIRANTE4D_GPU_ADAPTER";
pub const GPU_TIMESTAMPS_ENV: &str = "MIRANTE4D_GPU_TIMESTAMPS";

pub fn adapter_info_summary(info: &wgpu::AdapterInfo) -> String {
    format!(
        "{:?} {:?} {} driver={} {} pci={}",
        info.backend,
        info.device_type,
        info.name,
        info.driver,
        info.driver_info,
        info.device_pci_bus_id
    )
}

pub fn adapter_info_matches_name(info: &wgpu::AdapterInfo, requested: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() {
        return true;
    }
    let searchable = format!(
        "{} {} {} {:?} {:?}",
        info.name, info.driver, info.driver_info, info.backend, info.device_type
    )
    .to_ascii_lowercase();
    searchable.contains(&requested)
}

pub fn adapter_preference_score(info: &wgpu::AdapterInfo) -> i64 {
    if info.device_type == wgpu::DeviceType::Cpu || info.backend == wgpu::Backend::Noop {
        return i64::MIN;
    }

    let mut score = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 1_000,
        wgpu::DeviceType::IntegratedGpu => 100,
        wgpu::DeviceType::VirtualGpu => 50,
        wgpu::DeviceType::Other => 10,
        wgpu::DeviceType::Cpu => i64::MIN,
    };

    if is_nvidia_adapter(info) {
        score += 2_000;
    }

    score
        + match info.backend {
            wgpu::Backend::Vulkan | wgpu::Backend::Dx12 | wgpu::Backend::Metal => 50,
            wgpu::Backend::Gl => -50,
            wgpu::Backend::BrowserWebGpu => -200,
            wgpu::Backend::Noop => i64::MIN / 2,
        }
}

pub fn renderer_required_limits_for_adapter(
    adapter: &wgpu::Adapter,
) -> Result<wgpu::Limits, GpuRenderError> {
    renderer_required_limits_from_adapter_limits(&adapter.limits())
}

pub fn renderer_device_descriptor(
    adapter: &wgpu::Adapter,
    label: &'static str,
) -> Result<wgpu::DeviceDescriptor<'static>, GpuRenderError> {
    let required_limits = renderer_required_limits_for_adapter(adapter)?;
    let timestamp_features =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
    let required_features =
        if timestamp_queries_requested() && adapter.features().contains(timestamp_features) {
            timestamp_features
        } else {
            wgpu::Features::empty()
        };
    Ok(wgpu::DeviceDescriptor {
        label: Some(label),
        required_features,
        required_limits,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    })
}

pub(super) async fn request_device(
    label: &'static str,
) -> Result<(wgpu::Device, wgpu::Queue, AdapterDiagnostics), GpuRenderError> {
    let instance = wgpu::Instance::default();
    let adapter = select_renderer_adapter(&instance).await?;
    let adapter_info = adapter.get_info();
    validate_renderer_adapter(&adapter)?;
    let adapter_limits = adapter.limits();
    let device_descriptor = renderer_device_descriptor(&adapter, label)?;
    let timestamp_queries_requested = timestamp_queries_requested();
    let timestamp_features =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
    let adapter_timestamp_queries_supported = adapter.features().contains(timestamp_features);
    let required_features = device_descriptor.required_features;
    let required_limits = device_descriptor.required_limits.clone();
    let adapter_diagnostics = AdapterDiagnostics {
        name: adapter_info.name.clone(),
        backend: format!("{:?}", adapter_info.backend),
        device_type: format!("{:?}", adapter_info.device_type),
        driver: adapter_info.driver.clone(),
        driver_info: adapter_info.driver_info.clone(),
        timestamp_queries_supported: adapter_timestamp_queries_supported,
        timestamp_queries_requested,
        timestamp_queries_enabled: required_features.contains(timestamp_features),
        adapter_limits: GpuLimitDiagnostics::from(&adapter_limits),
        requested_limits: GpuLimitDiagnostics::from(&required_limits),
    };

    let (device, queue) = adapter
        .request_device(&device_descriptor)
        .await
        .map_err(|err| GpuRenderError::RequestDevice(err.to_string()))?;
    Ok((device, queue, adapter_diagnostics))
}

pub(super) fn diagnostics_for_existing_device(
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
) -> Result<AdapterDiagnostics, GpuRenderError> {
    validate_renderer_adapter(adapter)?;
    let adapter_info = adapter.get_info();
    let adapter_limits = adapter.limits();
    let requested_limits = device.limits();
    validate_existing_device_limits(&adapter_limits, &requested_limits)?;
    let timestamp_queries_requested = timestamp_queries_requested();
    let timestamp_features =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
    let adapter_timestamp_queries_supported = adapter.features().contains(timestamp_features);
    Ok(AdapterDiagnostics {
        name: adapter_info.name.clone(),
        backend: format!("{:?}", adapter_info.backend),
        device_type: format!("{:?}", adapter_info.device_type),
        driver: adapter_info.driver.clone(),
        driver_info: adapter_info.driver_info.clone(),
        timestamp_queries_supported: adapter_timestamp_queries_supported,
        timestamp_queries_requested,
        timestamp_queries_enabled: timestamp_queries_requested
            && device.features().contains(timestamp_features),
        adapter_limits: GpuLimitDiagnostics::from(&adapter_limits),
        requested_limits: GpuLimitDiagnostics::from(&requested_limits),
    })
}

pub(super) fn timestamp_queries_requested() -> bool {
    std::env::var(GPU_TIMESTAMPS_ENV)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            !(normalized.is_empty()
                || normalized == "0"
                || normalized == "false"
                || normalized == "off"
                || normalized == "no")
        })
        .unwrap_or(false)
}

async fn select_renderer_adapter(
    instance: &wgpu::Instance,
) -> Result<wgpu::Adapter, GpuRenderError> {
    let adapters = instance
        .enumerate_adapters(wgpu::Backends::PRIMARY | wgpu::Backends::GL)
        .await;
    if adapters.is_empty() {
        return Err(GpuRenderError::AdapterUnavailable(
            "wgpu did not report any adapters".to_owned(),
        ));
    }

    let requested = std::env::var(GPU_ADAPTER_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let selected = adapters
        .iter()
        .filter_map(|adapter| {
            let info = adapter.get_info();
            if let Some(requested) = &requested
                && !adapter_info_matches_name(&info, requested)
            {
                return None;
            }
            if validate_renderer_adapter(adapter).is_err() {
                return None;
            }
            Some((adapter_preference_score(&info), adapter.clone()))
        })
        .max_by_key(|(score, _adapter)| *score)
        .map(|(_score, adapter)| adapter);

    selected.ok_or_else(|| {
        let available = adapters
            .iter()
            .map(|adapter| adapter_info_summary(&adapter.get_info()))
            .collect::<Vec<_>>()
            .join("; ");
        let requested = requested
            .map(|value| format!(" matching {GPU_ADAPTER_ENV}={value:?}"))
            .unwrap_or_default();
        GpuRenderError::AdapterUnavailable(format!(
            "no usable non-CPU adapter{requested}; available adapters: {available}"
        ))
    })
}

fn validate_renderer_adapter(adapter: &wgpu::Adapter) -> Result<(), GpuRenderError> {
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(GpuRenderError::CpuAdapterOnly(info.name));
    }
    renderer_required_limits_for_adapter(adapter)?;
    Ok(())
}

fn renderer_required_limits_from_adapter_limits(
    adapter_limits: &wgpu::Limits,
) -> Result<wgpu::Limits, GpuRenderError> {
    validate_supported_u64_limit(
        "max_buffer_size",
        REQUIRED_MAX_BUFFER_SIZE,
        adapter_limits.max_buffer_size,
    )?;
    validate_supported_u64_limit(
        "max_storage_buffer_binding_size",
        REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE,
        adapter_limits.max_storage_buffer_binding_size,
    )?;
    validate_supported_u32_limit(
        "max_storage_buffers_per_shader_stage",
        REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE,
        adapter_limits.max_storage_buffers_per_shader_stage,
    )?;
    Ok(adapter_limits.clone())
}

fn validate_existing_device_limits(
    adapter_limits: &wgpu::Limits,
    requested_limits: &wgpu::Limits,
) -> Result<(), GpuRenderError> {
    let required = renderer_required_limits_from_adapter_limits(adapter_limits)?;
    validate_requested_u64_limit(
        "max_buffer_size",
        required.max_buffer_size.min(REQUIRED_MAX_BUFFER_SIZE),
        requested_limits.max_buffer_size,
    )?;
    validate_requested_u64_limit(
        "max_storage_buffer_binding_size",
        required
            .max_storage_buffer_binding_size
            .min(REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE),
        requested_limits.max_storage_buffer_binding_size,
    )?;
    validate_requested_u32_limit(
        "max_storage_buffers_per_shader_stage",
        required
            .max_storage_buffers_per_shader_stage
            .min(REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE),
        requested_limits.max_storage_buffers_per_shader_stage,
    )?;
    Ok(())
}

fn validate_supported_u64_limit(
    limit: &'static str,
    required: u64,
    supported: u64,
) -> Result<(), GpuRenderError> {
    if supported < required {
        return Err(GpuRenderError::RequiredLimitUnsupported {
            limit,
            required,
            supported,
        });
    }
    Ok(())
}

fn validate_supported_u32_limit(
    limit: &'static str,
    required: u32,
    supported: u32,
) -> Result<(), GpuRenderError> {
    if supported < required {
        return Err(GpuRenderError::RequiredLimitUnsupported {
            limit,
            required: u64::from(required),
            supported: u64::from(supported),
        });
    }
    Ok(())
}

fn validate_requested_u64_limit(
    limit: &'static str,
    required: u64,
    actual: u64,
) -> Result<(), GpuRenderError> {
    if actual < required {
        return Err(GpuRenderError::DeviceLimitTooLow {
            limit,
            required,
            actual,
        });
    }
    Ok(())
}

fn validate_requested_u32_limit(
    limit: &'static str,
    required: u32,
    actual: u32,
) -> Result<(), GpuRenderError> {
    if actual < required {
        return Err(GpuRenderError::DeviceLimitTooLow {
            limit,
            required: u64::from(required),
            actual: u64::from(actual),
        });
    }
    Ok(())
}

fn is_nvidia_adapter(info: &wgpu::AdapterInfo) -> bool {
    if info.vendor & 0xffff == 0x10de {
        return true;
    }
    let name = info.name.to_ascii_lowercase();
    name.contains("nvidia")
        || name.contains("geforce")
        || name.contains("rtx")
        || name.contains("quadro")
        || name.contains("tesla")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_preference_prefers_nvidia_discrete_over_integrated_gpu() {
        let nvidia = adapter_info(
            "NVIDIA GeForce RTX 3070 Ti Laptop GPU",
            0x10de,
            wgpu::DeviceType::DiscreteGpu,
            wgpu::Backend::Vulkan,
        );
        let integrated = adapter_info(
            "AMD Radeon 680M",
            0x1002,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::Backend::Vulkan,
        );

        assert!(adapter_preference_score(&nvidia) > adapter_preference_score(&integrated));
    }

    #[test]
    fn adapter_preference_rejects_cpu_adapters() {
        let cpu = adapter_info("llvmpipe", 0, wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan);

        assert_eq!(adapter_preference_score(&cpu), i64::MIN);
    }

    #[test]
    fn adapter_name_override_matches_case_insensitive_adapter_details() {
        let adapter = adapter_info(
            "NVIDIA GeForce RTX 3070 Ti Laptop GPU",
            0x10de,
            wgpu::DeviceType::DiscreteGpu,
            wgpu::Backend::Vulkan,
        );

        assert!(adapter_info_matches_name(&adapter, "rtx 3070"));
        assert!(adapter_info_matches_name(&adapter, "VULKAN"));
        assert!(!adapter_info_matches_name(&adapter, "radeon"));
    }

    #[test]
    fn renderer_required_limits_raise_storage_binding_envelope() {
        let adapter_limits = wgpu::Limits {
            max_buffer_size: REQUIRED_MAX_BUFFER_SIZE * 2,
            max_storage_buffer_binding_size: REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE * 2,
            max_storage_buffers_per_shader_stage: REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE + 1,
            ..Default::default()
        };

        let requested = renderer_required_limits_from_adapter_limits(&adapter_limits).unwrap();

        assert_eq!(
            requested.max_storage_buffer_binding_size,
            adapter_limits.max_storage_buffer_binding_size
        );
        assert!(
            requested.max_storage_buffer_binding_size >= REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE
        );
        assert!(requested.max_buffer_size >= REQUIRED_MAX_BUFFER_SIZE);
        assert!(
            requested.max_storage_buffers_per_shader_stage
                >= REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE
        );
    }

    #[test]
    fn renderer_required_limits_reject_insufficient_storage_binding_size() {
        let adapter_limits = wgpu::Limits {
            max_buffer_size: REQUIRED_MAX_BUFFER_SIZE,
            max_storage_buffer_binding_size: REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE - 1,
            max_storage_buffers_per_shader_stage: REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE,
            ..Default::default()
        };

        assert!(matches!(
            renderer_required_limits_from_adapter_limits(&adapter_limits),
            Err(GpuRenderError::RequiredLimitUnsupported {
                limit: "max_storage_buffer_binding_size",
                ..
            })
        ));
    }

    #[test]
    fn existing_device_limits_must_have_requested_renderer_envelope() {
        let adapter_limits = wgpu::Limits {
            max_buffer_size: REQUIRED_MAX_BUFFER_SIZE,
            max_storage_buffer_binding_size: REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE,
            max_storage_buffers_per_shader_stage: REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE,
            ..Default::default()
        };
        let requested_limits = wgpu::Limits {
            max_buffer_size: REQUIRED_MAX_BUFFER_SIZE,
            max_storage_buffer_binding_size: REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE - 1,
            max_storage_buffers_per_shader_stage: REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE,
            ..Default::default()
        };

        assert!(matches!(
            validate_existing_device_limits(&adapter_limits, &requested_limits),
            Err(GpuRenderError::DeviceLimitTooLow {
                limit: "max_storage_buffer_binding_size",
                ..
            })
        ));
    }

    fn adapter_info(
        name: &str,
        vendor: u32,
        device_type: wgpu::DeviceType,
        backend: wgpu::Backend,
    ) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: name.to_owned(),
            vendor,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend,
            subgroup_min_size: 32,
            subgroup_max_size: 32,
            transient_saves_memory: false,
        }
    }
}
