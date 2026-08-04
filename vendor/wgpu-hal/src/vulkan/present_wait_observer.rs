//! Opt-in final-present hook for Mirante4D's trusted local Vulkan campaign.
//!
//! The upstream WGPU path remains unchanged unless one live observer is
//! installed before instance creation. The observer supplies the present ID,
//! receives submission/rejection notifications, and correlates the result with
//! `VK_EXT_present_timing` feedback away from WGPU's presentation thread.

use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    ffi::c_void,
    mem::{self, offset_of, size_of},
    ptr,
};
use std::{ffi::CStr, sync::{Mutex, OnceLock}};

use ash::vk;

pub const PRESENT_TIMING_QUEUE_SIZE: u32 = 256;
pub const PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT: u32 = 0x0000_0004;

const PRESENT_TIMING_EXTENSION_NAME: &CStr =
    unsafe { CStr::from_bytes_with_nul_unchecked(b"VK_EXT_present_timing\0") };
const PRESENT_ID2_EXTENSION_NAME: &CStr =
    unsafe { CStr::from_bytes_with_nul_unchecked(b"VK_KHR_present_id2\0") };
const PRESENT_TIMING_FEATURES_STYPE: i32 = 1_000_208_000;
const PRESENT_TIMINGS_INFO_STYPE: i32 = 1_000_208_003;
const PRESENT_TIMING_INFO_STYPE: i32 = 1_000_208_004;
const PAST_PRESENTATION_TIMING_INFO_STYPE: i32 = 1_000_208_005;
const PAST_PRESENTATION_TIMING_PROPERTIES_STYPE: i32 = 1_000_208_006;
const PAST_PRESENTATION_TIMING_STYPE: i32 = 1_000_208_007;
const PRESENT_TIMING_SURFACE_CAPABILITIES_STYPE: i32 = 1_000_208_008;
const PRESENT_ID2_SURFACE_CAPABILITIES_STYPE: i32 = 1_000_479_000;
const PRESENT_ID2_INFO_STYPE: i32 = 1_000_479_001;
const PRESENT_ID2_FEATURES_STYPE: i32 = 1_000_479_002;
const SWAPCHAIN_CREATE_PRESENT_ID2_BIT: u32 = 0x0000_0040;
const SWAPCHAIN_CREATE_PRESENT_TIMING_BIT: u32 = 0x0000_0200;
const TIME_DOMAIN_PRESENT_STAGE_LOCAL: i32 = 1_000_208_000;
const TIME_DOMAIN_SWAPCHAIN_LOCAL: i32 = 1_000_208_001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTimingConfiguration {
    pub present_id2_supported: bool,
    pub present_timing_supported: bool,
    pub present_stage_queries: u32,
    pub queue_size: u32,
    pub time_domain: vk::TimeDomainKHR,
    pub time_domain_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationReservation {
    pub present_id: u64,
    pub time_domain_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTimingRecord {
    pub present_id: u64,
    pub present_stage_count: u32,
    pub stage: u32,
    pub time_ns: u64,
    pub time_domain: vk::TimeDomainKHR,
    pub time_domain_id: u64,
    pub report_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationTimingQuery {
    pub timing_properties_counter: u64,
    pub time_domains_counter: u64,
    pub records: Vec<PresentationTimingRecord>,
    pub incomplete: bool,
}

#[repr(C)]
pub(crate) struct PhysicalDevicePresentId2Features {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_id2: vk::Bool32,
}

impl Default for PhysicalDevicePresentId2Features {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_ID2_FEATURES_STYPE),
            p_next: ptr::null_mut(),
            present_id2: vk::FALSE,
        }
    }
}

#[repr(C)]
pub(crate) struct PhysicalDevicePresentTimingFeatures {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_timing: vk::Bool32,
    pub present_at_absolute_time: vk::Bool32,
    pub present_at_relative_time: vk::Bool32,
}

impl Default for PhysicalDevicePresentTimingFeatures {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_TIMING_FEATURES_STYPE),
            p_next: ptr::null_mut(),
            present_timing: vk::FALSE,
            present_at_absolute_time: vk::FALSE,
            present_at_relative_time: vk::FALSE,
        }
    }
}

#[repr(C)]
struct SurfaceCapabilitiesPresentId2 {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    present_id2_supported: vk::Bool32,
}

impl Default for SurfaceCapabilitiesPresentId2 {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_ID2_SURFACE_CAPABILITIES_STYPE),
            p_next: ptr::null_mut(),
            present_id2_supported: vk::FALSE,
        }
    }
}

#[repr(C)]
struct PresentTimingSurfaceCapabilities {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    present_timing_supported: vk::Bool32,
    present_at_absolute_time_supported: vk::Bool32,
    present_at_relative_time_supported: vk::Bool32,
    present_stage_queries: u32,
}

impl Default for PresentTimingSurfaceCapabilities {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_TIMING_SURFACE_CAPABILITIES_STYPE),
            p_next: ptr::null_mut(),
            present_timing_supported: vk::FALSE,
            present_at_absolute_time_supported: vk::FALSE,
            present_at_relative_time_supported: vk::FALSE,
            present_stage_queries: 0,
        }
    }
}

#[repr(C)]
pub(crate) struct PresentId2Info {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub swapchain_count: u32,
    pub p_present_ids: *const u64,
}

impl PresentId2Info {
    pub(crate) fn new(present_ids: &[u64]) -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_ID2_INFO_STYPE),
            p_next: ptr::null(),
            swapchain_count: present_ids.len() as u32,
            p_present_ids: present_ids.as_ptr(),
        }
    }
}

#[repr(C)]
pub(crate) struct PresentTimingInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    flags: u32,
    target_time: u64,
    time_domain_id: u64,
    present_stage_queries: u32,
    target_time_domain_present_stage: u32,
}

impl PresentTimingInfo {
    pub(crate) fn first_pixel_out(time_domain_id: u64) -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_TIMING_INFO_STYPE),
            p_next: ptr::null(),
            flags: 0,
            target_time: 0,
            time_domain_id,
            present_stage_queries: PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT,
            target_time_domain_present_stage: 0,
        }
    }
}

#[repr(C)]
pub(crate) struct PresentTimingsInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub swapchain_count: u32,
    pub p_timing_infos: *const PresentTimingInfo,
}

impl PresentTimingsInfo {
    pub(crate) fn new(timing_infos: &[PresentTimingInfo]) -> Self {
        Self {
            s_type: vk::StructureType::from_raw(PRESENT_TIMINGS_INFO_STYPE),
            p_next: ptr::null(),
            swapchain_count: timing_infos.len() as u32,
            p_timing_infos: timing_infos.as_ptr(),
        }
    }
}

#[repr(C)]
struct SwapchainTimingProperties {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    refresh_duration: u64,
    refresh_interval: u64,
}

impl Default for SwapchainTimingProperties {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(1_000_208_001),
            p_next: ptr::null_mut(),
            refresh_duration: 0,
            refresh_interval: 0,
        }
    }
}

#[repr(C)]
struct SwapchainTimeDomainProperties {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    time_domain_count: u32,
    p_time_domains: *mut vk::TimeDomainKHR,
    p_time_domain_ids: *mut u64,
}

impl Default for SwapchainTimeDomainProperties {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(1_000_208_002),
            p_next: ptr::null_mut(),
            time_domain_count: 0,
            p_time_domains: ptr::null_mut(),
            p_time_domain_ids: ptr::null_mut(),
        }
    }
}

#[repr(C)]
struct PastPresentationTimingInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    flags: u32,
    swapchain: vk::SwapchainKHR,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PresentStageTime {
    stage: u32,
    time: u64,
}

#[repr(C)]
struct PastPresentationTiming {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    present_id: u64,
    target_time: u64,
    present_stage_count: u32,
    p_present_stages: *mut PresentStageTime,
    time_domain: vk::TimeDomainKHR,
    time_domain_id: u64,
    report_complete: vk::Bool32,
}

#[repr(C)]
struct PastPresentationTimingProperties {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    timing_properties_counter: u64,
    time_domains_counter: u64,
    presentation_timing_count: u32,
    p_presentation_timings: *mut PastPresentationTiming,
}

// Keep the hand-written revision-3 ABI fail-closed against the Khronos
// registry on the only qualified target for this hook: 64-bit Linux Vulkan.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(PRESENT_TIMING_FEATURES_STYPE == 1_000_208_000);
    assert!(PRESENT_TIMINGS_INFO_STYPE == 1_000_208_003);
    assert!(PRESENT_TIMING_INFO_STYPE == 1_000_208_004);
    assert!(PAST_PRESENTATION_TIMING_INFO_STYPE == 1_000_208_005);
    assert!(PAST_PRESENTATION_TIMING_PROPERTIES_STYPE == 1_000_208_006);
    assert!(PAST_PRESENTATION_TIMING_STYPE == 1_000_208_007);
    assert!(PRESENT_TIMING_SURFACE_CAPABILITIES_STYPE == 1_000_208_008);
    assert!(PRESENT_ID2_SURFACE_CAPABILITIES_STYPE == 1_000_479_000);
    assert!(PRESENT_ID2_INFO_STYPE == 1_000_479_001);
    assert!(PRESENT_ID2_FEATURES_STYPE == 1_000_479_002);
    assert!(SWAPCHAIN_CREATE_PRESENT_ID2_BIT == 0x0000_0040);
    assert!(SWAPCHAIN_CREATE_PRESENT_TIMING_BIT == 0x0000_0200);
    assert!(PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT == 0x0000_0004);

    assert!(size_of::<PhysicalDevicePresentId2Features>() == 24);
    assert!(size_of::<PhysicalDevicePresentTimingFeatures>() == 32);
    assert!(size_of::<SurfaceCapabilitiesPresentId2>() == 24);
    assert!(size_of::<PresentTimingSurfaceCapabilities>() == 32);
    assert!(size_of::<PresentId2Info>() == 32);
    assert!(size_of::<PresentTimingInfo>() == 48);
    assert!(offset_of!(PresentTimingInfo, target_time) == 24);
    assert!(offset_of!(PresentTimingInfo, time_domain_id) == 32);
    assert!(offset_of!(PresentTimingInfo, present_stage_queries) == 40);
    assert!(size_of::<PresentTimingsInfo>() == 32);
    assert!(size_of::<SwapchainTimingProperties>() == 32);
    assert!(size_of::<SwapchainTimeDomainProperties>() == 40);
    assert!(size_of::<PastPresentationTimingInfo>() == 32);
    assert!(size_of::<PresentStageTime>() == 16);
    assert!(size_of::<PastPresentationTiming>() == 72);
    assert!(offset_of!(PastPresentationTiming, present_id) == 16);
    assert!(offset_of!(PastPresentationTiming, present_stage_count) == 32);
    assert!(offset_of!(PastPresentationTiming, p_present_stages) == 40);
    assert!(offset_of!(PastPresentationTiming, time_domain) == 48);
    assert!(offset_of!(PastPresentationTiming, time_domain_id) == 56);
    assert!(offset_of!(PastPresentationTiming, report_complete) == 64);
    assert!(size_of::<PastPresentationTimingProperties>() == 48);
    assert!(offset_of!(PastPresentationTimingProperties, p_presentation_timings) == 40);
};

type SetTimingQueueSizeFn = unsafe extern "system" fn(
    vk::Device,
    vk::SwapchainKHR,
    u32,
) -> vk::Result;
type GetSwapchainTimingPropertiesFn = unsafe extern "system" fn(
    vk::Device,
    vk::SwapchainKHR,
    *mut SwapchainTimingProperties,
    *mut u64,
) -> vk::Result;
type GetSwapchainTimeDomainsFn = unsafe extern "system" fn(
    vk::Device,
    vk::SwapchainKHR,
    *mut SwapchainTimeDomainProperties,
    *mut u64,
) -> vk::Result;
type GetPastPresentationTimingFn = unsafe extern "system" fn(
    vk::Device,
    *const PastPresentationTimingInfo,
    *mut PastPresentationTimingProperties,
) -> vk::Result;

#[derive(Clone)]
pub struct PresentationTimingDevice {
    device: vk::Device,
    set_queue_size: SetTimingQueueSizeFn,
    get_timing_properties: GetSwapchainTimingPropertiesFn,
    get_time_domains: GetSwapchainTimeDomainsFn,
    get_past_timing: GetPastPresentationTimingFn,
}

impl PresentationTimingDevice {
    /// Load the four revision-3 device entry points from a device that enabled
    /// `VK_EXT_present_timing` and its dependencies.
    ///
    /// # Safety
    ///
    /// `device` must belong to `instance` and remain live while this wrapper is
    /// used.
    pub unsafe fn new(instance: &ash::Instance, device: &ash::Device) -> Result<Self, &'static str> {
        unsafe fn load(
            instance: &ash::Instance,
            device: vk::Device,
            name: &'static [u8],
        ) -> Option<unsafe extern "system" fn()> {
            unsafe { instance.get_device_proc_addr(device, name.as_ptr().cast()) }
        }

        let device_handle = device.handle();
        let set_queue_size = unsafe {
            mem::transmute::<unsafe extern "system" fn(), SetTimingQueueSizeFn>(load(
                instance,
                device_handle,
                b"vkSetSwapchainPresentTimingQueueSizeEXT\0",
            ).ok_or("vkSetSwapchainPresentTimingQueueSizeEXT is unavailable")?)
        };
        let get_timing_properties = unsafe {
            mem::transmute::<unsafe extern "system" fn(), GetSwapchainTimingPropertiesFn>(load(
                instance,
                device_handle,
                b"vkGetSwapchainTimingPropertiesEXT\0",
            ).ok_or("vkGetSwapchainTimingPropertiesEXT is unavailable")?)
        };
        let get_time_domains = unsafe {
            mem::transmute::<unsafe extern "system" fn(), GetSwapchainTimeDomainsFn>(load(
                instance,
                device_handle,
                b"vkGetSwapchainTimeDomainPropertiesEXT\0",
            ).ok_or("vkGetSwapchainTimeDomainPropertiesEXT is unavailable")?)
        };
        let get_past_timing = unsafe {
            mem::transmute::<unsafe extern "system" fn(), GetPastPresentationTimingFn>(load(
                instance,
                device_handle,
                b"vkGetPastPresentationTimingEXT\0",
            ).ok_or("vkGetPastPresentationTimingEXT is unavailable")?)
        };
        Ok(Self {
            device: device_handle,
            set_queue_size,
            get_timing_properties,
            get_time_domains,
            get_past_timing,
        })
    }

    /// Allocate the bounded result queue and select one driver-reported time
    /// domain for a new swapchain.
    ///
    /// # Safety
    ///
    /// `swapchain` must be live, belong to this device, and have both observer
    /// create flags.
    pub unsafe fn configure_swapchain(
        &self,
        swapchain: vk::SwapchainKHR,
    ) -> Result<(vk::TimeDomainKHR, u64), vk::Result> {
        let result = unsafe {
            (self.set_queue_size)(self.device, swapchain, PRESENT_TIMING_QUEUE_SIZE)
        };
        if result != vk::Result::SUCCESS {
            return Err(result);
        }
        let mut domains_counter = 0_u64;
        let mut properties = SwapchainTimeDomainProperties::default();
        let result = unsafe {
            (self.get_time_domains)(
                self.device,
                swapchain,
                &mut properties,
                &mut domains_counter,
            )
        };
        if result != vk::Result::SUCCESS && result != vk::Result::INCOMPLETE {
            return Err(result);
        }
        if properties.time_domain_count == 0 {
            return Err(vk::Result::ERROR_UNKNOWN);
        }
        let count = properties.time_domain_count as usize;
        let mut domains = vec![vk::TimeDomainKHR::default(); count];
        let mut ids = vec![0_u64; count];
        properties.p_time_domains = domains.as_mut_ptr();
        properties.p_time_domain_ids = ids.as_mut_ptr();
        let result = unsafe {
            (self.get_time_domains)(
                self.device,
                swapchain,
                &mut properties,
                &mut domains_counter,
            )
        };
        if result != vk::Result::SUCCESS || properties.time_domain_count as usize > count {
            return Err(result);
        }
        domains.truncate(properties.time_domain_count as usize);
        ids.truncate(properties.time_domain_count as usize);
        select_time_domain(&domains, &ids).ok_or(vk::Result::ERROR_UNKNOWN)
    }

    /// Query all currently available complete first-pixel-out records.
    ///
    /// # Safety
    ///
    /// `swapchain` must be live and belong to this device. One caller must own
    /// its timing-query stream until all admitted records are drained.
    pub unsafe fn query_past(
        &self,
        swapchain: vk::SwapchainKHR,
    ) -> Result<PresentationTimingQuery, vk::Result> {
        let capacity = PRESENT_TIMING_QUEUE_SIZE as usize;
        let mut stages = vec![PresentStageTime::default(); capacity];
        let mut timings = (0..capacity)
            .map(|index| PastPresentationTiming {
                s_type: vk::StructureType::from_raw(PAST_PRESENTATION_TIMING_STYPE),
                p_next: ptr::null_mut(),
                present_id: 0,
                target_time: 0,
                present_stage_count: 1,
                p_present_stages: &mut stages[index],
                time_domain: vk::TimeDomainKHR::default(),
                time_domain_id: 0,
                report_complete: vk::FALSE,
            })
            .collect::<Vec<_>>();
        let info = PastPresentationTimingInfo {
            s_type: vk::StructureType::from_raw(PAST_PRESENTATION_TIMING_INFO_STYPE),
            p_next: ptr::null(),
            flags: 0,
            swapchain,
        };
        let mut properties = PastPresentationTimingProperties {
            s_type: vk::StructureType::from_raw(PAST_PRESENTATION_TIMING_PROPERTIES_STYPE),
            p_next: ptr::null_mut(),
            timing_properties_counter: 0,
            time_domains_counter: 0,
            presentation_timing_count: capacity as u32,
            p_presentation_timings: timings.as_mut_ptr(),
        };
        let result = unsafe { (self.get_past_timing)(self.device, &info, &mut properties) };
        if result != vk::Result::SUCCESS && result != vk::Result::INCOMPLETE {
            return Err(result);
        }
        if properties.presentation_timing_count as usize > capacity {
            return Err(vk::Result::INCOMPLETE);
        }
        let count = properties.presentation_timing_count as usize;
        let records = timings
            .into_iter()
            .take(count)
            .zip(stages)
            .map(|(timing, stage)| PresentationTimingRecord {
                present_id: timing.present_id,
                present_stage_count: timing.present_stage_count,
                stage: stage.stage,
                time_ns: stage.time,
                time_domain: timing.time_domain,
                time_domain_id: timing.time_domain_id,
                report_complete: timing.report_complete == vk::TRUE,
            })
            .collect();
        Ok(PresentationTimingQuery {
            timing_properties_counter: properties.timing_properties_counter,
            time_domains_counter: properties.time_domains_counter,
            records,
            incomplete: result == vk::Result::INCOMPLETE,
        })
    }

    /// Read refresh facts when the driver has made them available.
    ///
    /// # Safety
    ///
    /// `swapchain` must be live and belong to this device.
    pub unsafe fn timing_properties(
        &self,
        swapchain: vk::SwapchainKHR,
    ) -> Result<Option<(u64, u64, u64)>, vk::Result> {
        let mut properties = SwapchainTimingProperties::default();
        let mut counter = 0_u64;
        let result = unsafe {
            (self.get_timing_properties)(self.device, swapchain, &mut properties, &mut counter)
        };
        match result {
            vk::Result::SUCCESS => Ok(Some((
                properties.refresh_duration,
                properties.refresh_interval,
                counter,
            ))),
            vk::Result::NOT_READY => Ok(None),
            error => Err(error),
        }
    }
}

fn select_time_domain(
    domains: &[vk::TimeDomainKHR],
    ids: &[u64],
) -> Option<(vk::TimeDomainKHR, u64)> {
    if domains.len() != ids.len() {
        return None;
    }
    [
        vk::TimeDomainKHR::CLOCK_MONOTONIC,
        vk::TimeDomainKHR::CLOCK_MONOTONIC_RAW,
        vk::TimeDomainKHR::from_raw(TIME_DOMAIN_PRESENT_STAGE_LOCAL),
        vk::TimeDomainKHR::from_raw(TIME_DOMAIN_SWAPCHAIN_LOCAL),
        vk::TimeDomainKHR::DEVICE,
        vk::TimeDomainKHR::QUERY_PERFORMANCE_COUNTER,
    ]
    .into_iter()
    .find_map(|preferred| {
        domains
            .iter()
            .zip(ids)
            .find(|(domain, _)| **domain == preferred)
            .map(|(domain, id)| (*domain, *id))
    })
    .or_else(|| domains.first().copied().zip(ids.first().copied()))
}

pub(crate) const fn present_timing_extension_name() -> &'static CStr {
    PRESENT_TIMING_EXTENSION_NAME
}

pub(crate) const fn present_id2_extension_name() -> &'static CStr {
    PRESENT_ID2_EXTENSION_NAME
}

pub(crate) const fn observer_swapchain_flags() -> vk::SwapchainCreateFlagsKHR {
    vk::SwapchainCreateFlagsKHR::from_raw(
        SWAPCHAIN_CREATE_PRESENT_ID2_BIT | SWAPCHAIN_CREATE_PRESENT_TIMING_BIT,
    )
}

pub(crate) unsafe fn query_surface_configuration(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> Result<PresentationTimingConfiguration, vk::Result> {
    let loader = ash::khr::get_surface_capabilities2::Instance::new(entry, instance);
    let surface_info = vk::PhysicalDeviceSurfaceInfo2KHR::default().surface(surface);
    let mut timing = PresentTimingSurfaceCapabilities::default();
    let mut id2 = SurfaceCapabilitiesPresentId2::default();
    id2.p_next = ptr::from_mut(&mut timing).cast();
    let mut capabilities = vk::SurfaceCapabilities2KHR::default();
    capabilities.p_next = ptr::from_mut(&mut id2).cast();
    unsafe {
        loader.get_physical_device_surface_capabilities2(
            physical_device,
            &surface_info,
            &mut capabilities,
        )?
    };
    Ok(PresentationTimingConfiguration {
        present_id2_supported: id2.present_id2_supported == vk::TRUE,
        present_timing_supported: timing.present_timing_supported == vk::TRUE,
        present_stage_queries: timing.present_stage_queries,
        queue_size: PRESENT_TIMING_QUEUE_SIZE,
        time_domain: vk::TimeDomainKHR::default(),
        time_domain_id: 0,
    })
}

/// Observer installed by a trusted local presentation measurement.
pub trait PresentationWaitObserver: Send + Sync {
    /// A newly created swapchain passed the surface-capability query, allocated
    /// its timing queue, and selected the reported time domain.
    fn presentation_timing_configured(
        &self,
        swapchain: vk::SwapchainKHR,
        configuration: PresentationTimingConfiguration,
    );

    /// Reserve a strictly increasing ID for this swapchain presentation.
    fn reserve_present(
        &self,
        swapchain: vk::SwapchainKHR,
    ) -> Option<PresentationReservation>;

    /// The ID was accepted by `vkQueuePresentKHR`.
    fn present_submitted(&self, swapchain: vk::SwapchainKHR, present_id: u64);

    /// The corresponding `vkQueuePresentKHR` call failed.
    fn present_rejected(
        &self,
        swapchain: vk::SwapchainKHR,
        present_id: u64,
        result: vk::Result,
    );

    /// Finish or abandon outstanding host waits before this handle is destroyed.
    fn before_swapchain_destroy(&self, swapchain: vk::SwapchainKHR);
}

fn observer_slot() -> &'static Mutex<Option<Weak<dyn PresentationWaitObserver>>> {
    static SLOT: OnceLock<Mutex<Option<Weak<dyn PresentationWaitObserver>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Install one weak, process-local observer before the Vulkan device is opened.
pub fn install_presentation_wait_observer(
    observer: &Arc<dyn PresentationWaitObserver>,
) -> Result<(), &'static str> {
    let mut slot = observer_slot()
        .lock()
        .map_err(|_| "presentation-wait observer lock is poisoned")?;
    if slot.as_ref().and_then(Weak::upgrade).is_some() {
        return Err("a live presentation-wait observer is already installed");
    }
    *slot = Some(Arc::downgrade(observer));
    Ok(())
}

pub(crate) fn installed() -> bool {
    observer_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(Weak::upgrade))
        .is_some()
}

pub(crate) struct ReservedPresent {
    observer: Arc<dyn PresentationWaitObserver>,
    swapchain: vk::SwapchainKHR,
    present_id: u64,
    time_domain_id: u64,
}

impl ReservedPresent {
    pub(crate) fn present_id(&self) -> u64 {
        self.present_id
    }

    pub(crate) fn time_domain_id(&self) -> u64 {
        self.time_domain_id
    }

    pub(crate) fn submitted(self) {
        self.observer
            .present_submitted(self.swapchain, self.present_id);
    }

    pub(crate) fn rejected(self, result: vk::Result) {
        self.observer
            .present_rejected(self.swapchain, self.present_id, result);
    }
}

pub(crate) fn reserve(swapchain: vk::SwapchainKHR) -> Option<ReservedPresent> {
    let observer = observer_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(Weak::upgrade))?;
    let reservation = observer.reserve_present(swapchain)?;
    Some(ReservedPresent {
        observer,
        swapchain,
        present_id: reservation.present_id,
        time_domain_id: reservation.time_domain_id,
    })
}

pub(crate) fn before_swapchain_destroy(swapchain: vk::SwapchainKHR) {
    if let Some(observer) = observer_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(Weak::upgrade))
    {
        observer.before_swapchain_destroy(swapchain);
    }
}

pub(crate) fn presentation_timing_configured(
    swapchain: vk::SwapchainKHR,
    configuration: PresentationTimingConfiguration,
) {
    if let Some(observer) = observer_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(Weak::upgrade))
    {
        observer.presentation_timing_configured(swapchain, configuration);
    }
}
