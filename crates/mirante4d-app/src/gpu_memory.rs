//! Native memory facts for the exact adapter selected by eframe.
//!
//! wgpu intentionally exposes portable limits rather than physical-memory
//! capacity. The native Linux product therefore performs one backend-qualified
//! query through the selected adapter's Vulkan HAL handle. Other backends and
//! failed queries remain explicit unknowns; they never trigger a second
//! adapter selection or a process-command heuristic.

use std::fmt;

use ash::vk;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpuMemoryModel {
    Dedicated,
    SharedOrUnknown,
}

impl fmt::Display for GpuMemoryModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Dedicated => "dedicated",
            Self::SharedOrUnknown => "shared-or-unknown",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpuMemoryDiscoverySource {
    VulkanMemoryBudget,
    VulkanHeapProperties,
    Unavailable,
}

impl fmt::Display for GpuMemoryDiscoverySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::VulkanMemoryBudget => "vulkan-memory-budget",
            Self::VulkanHeapProperties => "vulkan-heap-properties",
            Self::Unavailable => "unavailable",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GpuMemoryDiscoveryFailure {
    UnsupportedBackend(wgpu::Backend),
    VulkanHalUnavailable,
    NoDeviceLocalHeap,
    ArithmeticOverflow,
}

impl fmt::Display for GpuMemoryDiscoveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBackend(backend) => {
                write!(
                    formatter,
                    "backend {backend:?} has no qualified memory provider"
                )
            }
            Self::VulkanHalUnavailable => {
                formatter.write_str("selected Vulkan adapter did not expose its HAL handle")
            }
            Self::NoDeviceLocalHeap => {
                formatter.write_str("selected Vulkan adapter reported no device-local heap")
            }
            Self::ArithmeticOverflow => {
                formatter.write_str("selected adapter memory heap sum overflowed")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedAdapterMemoryFacts {
    adapter_name: String,
    backend: wgpu::Backend,
    device_type: wgpu::DeviceType,
    memory_model: GpuMemoryModel,
    source: GpuMemoryDiscoverySource,
    device_local_bytes: Option<u64>,
    driver_budget_bytes: Option<u64>,
    driver_usage_bytes: Option<u64>,
    failure: Option<GpuMemoryDiscoveryFailure>,
}

impl SelectedAdapterMemoryFacts {
    #[cfg(test)]
    pub(crate) fn unavailable_for_tests() -> Self {
        Self {
            adapter_name: "test-unavailable".to_owned(),
            backend: wgpu::Backend::Noop,
            device_type: wgpu::DeviceType::Other,
            memory_model: GpuMemoryModel::SharedOrUnknown,
            source: GpuMemoryDiscoverySource::Unavailable,
            device_local_bytes: None,
            driver_budget_bytes: None,
            driver_usage_bytes: None,
            failure: Some(GpuMemoryDiscoveryFailure::UnsupportedBackend(
                wgpu::Backend::Noop,
            )),
        }
    }

    pub(crate) fn discover(adapter: &wgpu::Adapter) -> Self {
        let info = adapter.get_info();
        let memory_model = if info.device_type == wgpu::DeviceType::DiscreteGpu {
            GpuMemoryModel::Dedicated
        } else {
            GpuMemoryModel::SharedOrUnknown
        };
        if info.backend != wgpu::Backend::Vulkan {
            let backend = info.backend;
            return Self::unavailable(
                info,
                memory_model,
                GpuMemoryDiscoveryFailure::UnsupportedBackend(backend),
            );
        }

        #[cfg(target_os = "linux")]
        {
            // SAFETY: `as_hal` returns a ref-counted guard for this exact wgpu
            // adapter. We only issue read-only physical-device property
            // queries while the guard is alive and never destroy or retain raw
            // Vulkan handles.
            let Some(hal_adapter) = (unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }) else {
                return Self::unavailable(
                    info,
                    memory_model,
                    GpuMemoryDiscoveryFailure::VulkanHalUnavailable,
                );
            };
            Self::discover_vulkan(info, memory_model, &hal_adapter)
        }

        #[cfg(not(target_os = "linux"))]
        Self::unavailable(
            info,
            memory_model,
            GpuMemoryDiscoveryFailure::UnsupportedBackend(wgpu::Backend::Vulkan),
        )
    }

    #[cfg(target_os = "linux")]
    fn discover_vulkan(
        info: wgpu::AdapterInfo,
        memory_model: GpuMemoryModel,
        adapter: &wgpu::hal::vulkan::Adapter,
    ) -> Self {
        let supports_budget = adapter
            .physical_device_capabilities()
            .supports_extension(ash::ext::memory_budget::NAME);
        let physical_device = adapter.raw_physical_device();
        let instance = adapter.shared_instance().raw_instance();

        let (memory, budget) = if supports_budget {
            let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
            let mut properties =
                vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
            // SAFETY: `physical_device` and `instance` belong to the guarded
            // selected HAL adapter, and both output structures live for the
            // duration of this synchronous Vulkan query.
            unsafe {
                instance.get_physical_device_memory_properties2(physical_device, &mut properties);
            }
            (properties.memory_properties, Some(budget))
        } else {
            // SAFETY: the raw physical-device handle belongs to the guarded
            // selected HAL adapter and the query only returns value data.
            (
                unsafe { instance.get_physical_device_memory_properties(physical_device) },
                None,
            )
        };

        let heap_count = memory.memory_heap_count as usize;
        let mut device_local_bytes = 0_u64;
        let mut driver_budget_bytes = 0_u64;
        let mut driver_usage_bytes = 0_u64;
        let mut device_local_heaps = 0_usize;
        for heap_index in 0..heap_count {
            let heap = memory.memory_heaps[heap_index];
            if !heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
                continue;
            }
            device_local_heaps += 1;
            let Some(total) = device_local_bytes.checked_add(heap.size) else {
                return Self::unavailable(
                    info,
                    memory_model,
                    GpuMemoryDiscoveryFailure::ArithmeticOverflow,
                );
            };
            device_local_bytes = total;
            if let Some(budget) = budget.as_ref() {
                let Some(total_budget) =
                    driver_budget_bytes.checked_add(budget.heap_budget[heap_index])
                else {
                    return Self::unavailable(
                        info,
                        memory_model,
                        GpuMemoryDiscoveryFailure::ArithmeticOverflow,
                    );
                };
                let Some(total_usage) =
                    driver_usage_bytes.checked_add(budget.heap_usage[heap_index])
                else {
                    return Self::unavailable(
                        info,
                        memory_model,
                        GpuMemoryDiscoveryFailure::ArithmeticOverflow,
                    );
                };
                driver_budget_bytes = total_budget;
                driver_usage_bytes = total_usage;
            }
        }
        if device_local_heaps == 0 || device_local_bytes == 0 {
            return Self::unavailable(
                info,
                memory_model,
                GpuMemoryDiscoveryFailure::NoDeviceLocalHeap,
            );
        }

        let valid_budget = supports_budget
            .then_some(driver_budget_bytes)
            .filter(|bytes| *bytes != 0);
        let valid_usage = valid_budget.map(|_| driver_usage_bytes);
        Self {
            adapter_name: info.name,
            backend: info.backend,
            device_type: info.device_type,
            memory_model,
            source: if valid_budget.is_some() {
                GpuMemoryDiscoverySource::VulkanMemoryBudget
            } else {
                GpuMemoryDiscoverySource::VulkanHeapProperties
            },
            device_local_bytes: Some(device_local_bytes),
            driver_budget_bytes: valid_budget,
            driver_usage_bytes: valid_usage,
            failure: None,
        }
    }

    fn unavailable(
        info: wgpu::AdapterInfo,
        memory_model: GpuMemoryModel,
        failure: GpuMemoryDiscoveryFailure,
    ) -> Self {
        Self {
            adapter_name: info.name,
            backend: info.backend,
            device_type: info.device_type,
            memory_model,
            source: GpuMemoryDiscoverySource::Unavailable,
            device_local_bytes: None,
            driver_budget_bytes: None,
            driver_usage_bytes: None,
            failure: Some(failure),
        }
    }

    pub(crate) fn recommended_capacity_bytes(&self) -> Option<u64> {
        (self.memory_model == GpuMemoryModel::Dedicated)
            .then_some(self.device_local_bytes)
            .flatten()
            .map(|heap| {
                self.driver_budget_bytes
                    .map_or(heap, |budget| heap.min(budget))
            })
    }

    pub(crate) fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub(crate) const fn backend(&self) -> wgpu::Backend {
        self.backend
    }

    pub(crate) const fn device_type(&self) -> wgpu::DeviceType {
        self.device_type
    }

    pub(crate) const fn memory_model(&self) -> GpuMemoryModel {
        self.memory_model
    }

    pub(crate) const fn source(&self) -> GpuMemoryDiscoverySource {
        self.source
    }

    pub(crate) const fn device_local_bytes(&self) -> Option<u64> {
        self.device_local_bytes
    }

    pub(crate) const fn driver_budget_bytes(&self) -> Option<u64> {
        self.driver_budget_bytes
    }

    pub(crate) const fn driver_usage_bytes(&self) -> Option<u64> {
        self.driver_usage_bytes
    }

    pub(crate) fn failure(&self) -> Option<&GpuMemoryDiscoveryFailure> {
        self.failure.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(heap: u64, budget: Option<u64>) -> SelectedAdapterMemoryFacts {
        SelectedAdapterMemoryFacts {
            adapter_name: "test".to_owned(),
            backend: wgpu::Backend::Vulkan,
            device_type: wgpu::DeviceType::DiscreteGpu,
            memory_model: GpuMemoryModel::Dedicated,
            source: GpuMemoryDiscoverySource::VulkanMemoryBudget,
            device_local_bytes: Some(heap),
            driver_budget_bytes: budget,
            driver_usage_bytes: Some(0),
            failure: None,
        }
    }

    #[test]
    fn recommendation_capacity_never_exceeds_heap_or_driver_budget() {
        assert_eq!(
            facts(8_000, Some(7_000)).recommended_capacity_bytes(),
            Some(7_000)
        );
        assert_eq!(
            facts(8_000, Some(9_000)).recommended_capacity_bytes(),
            Some(8_000)
        );
        assert_eq!(facts(8_000, None).recommended_capacity_bytes(), Some(8_000));
    }

    #[test]
    fn shared_heap_facts_are_not_misrepresented_as_dedicated_vram() {
        let mut shared = facts(8_000, Some(7_000));
        shared.memory_model = GpuMemoryModel::SharedOrUnknown;
        assert_eq!(shared.recommended_capacity_bytes(), None);
        assert_eq!(shared.device_local_bytes(), Some(8_000));
        assert_eq!(shared.driver_budget_bytes(), Some(7_000));
    }

    #[test]
    fn unavailable_discovery_retains_an_explicit_conservative_fallback() {
        let unavailable = SelectedAdapterMemoryFacts::unavailable_for_tests();
        assert_eq!(unavailable.source(), GpuMemoryDiscoverySource::Unavailable);
        assert_eq!(unavailable.recommended_capacity_bytes(), None);
        assert!(unavailable.device_local_bytes().is_none());
        assert!(unavailable.failure().is_some());
    }

    #[test]
    #[ignore = "requires the trusted native Vulkan workstation"]
    fn selected_vulkan_adapter_reports_typed_device_local_memory() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("the trusted workstation exposes its Vulkan adapter");
        let info = adapter.get_info();
        let facts = SelectedAdapterMemoryFacts::discover(&adapter);
        eprintln!("selected-adapter memory facts: {facts:?}");

        assert_eq!(facts.adapter_name(), info.name);
        assert_eq!(facts.backend(), wgpu::Backend::Vulkan);
        assert!(
            facts
                .device_local_bytes()
                .is_some_and(|bytes| bytes >= 256 * 1024 * 1024)
        );
        assert!(facts.recommended_capacity_bytes().is_some());
        assert!(facts.failure().is_none());
        assert!(matches!(
            facts.source(),
            GpuMemoryDiscoverySource::VulkanMemoryBudget
                | GpuMemoryDiscoverySource::VulkanHeapProperties
        ));
    }
}
