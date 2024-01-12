use std::sync::{Arc, RwLock};
use wgpu::{Adapter, DeviceType, Limits};

use super::{BufferDescriptor, BufferPool, GPUBuffer, PoolError};

pub const MAX_BUFFER_SIZE: u64 = (2 << 29) - 1;

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("Failed to acquire device with error: {0:?}")]
    DeviceAcquisitionFailed(#[from] wgpu::RequestDeviceError),
    #[error("Failed to get adapter.")]
    AdapterRequestFailed,
    #[error("Failed to create buffer with error: {0:?}")]
    BufferCreationFailed(#[from] PoolError),
}

/// # Device
///
/// A device is a handle to a physical GPU.
/// It is used to create resources and submit commands to the GPU.
///
/// Currently, WebGPU doesn't support multiple devices. Ordinal should always
/// be 0.
#[derive(Clone)]
pub struct Device {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    ordinal: u32,
    buffer_pool: Arc<RwLock<BufferPool>>,
}

impl std::ops::Deref for Device {
    type Target = wgpu::Device;

    fn deref(&self) -> &Self::Target {
        &self.device
    }
}

//Device creation impls
impl Device {
    pub async fn new() -> Result<Self, DeviceError> {
        #[cfg(target_arch = "wasm32")]
        let adapter = Self::select_adapter().await;
        #[cfg(not(target_arch = "wasm32"))]
        let adapter = Self::select_adapter()?;

        #[allow(unused_mut)]
        let mut features = wgpu::Features::default();
        #[cfg(feature = "gpu-profiling")]
        {
            features |= wgpu::Features::TIMESTAMP_QUERY;
        }

        let mut device_descriptor = wgpu::DeviceDescriptor {
            label: Some("ratchet"),
            features,
            limits: Limits {
                max_buffer_size: MAX_BUFFER_SIZE,
                max_storage_buffer_binding_size: MAX_BUFFER_SIZE as u32,
                ..Default::default()
            },
        };
        let device_request = adapter.request_device(&device_descriptor, None).await;
        let (device, queue) = if let Err(e) = device_request {
            log::warn!(
                "Failed to acq. device, trying again with reduced limits: {:?}",
                e
            );
            device_descriptor.limits = adapter.limits();
            adapter.request_device(&device_descriptor, None).await
        } else {
            device_request
        }?;

        Ok(Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            ordinal: 0, //TODO: Support multiple devices
            buffer_pool: Arc::new(RwLock::new(BufferPool::new())),
        })
    }

    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }

    #[cfg(target_arch = "wasm32")]
    async fn select_adapter() -> Adapter {
        let instance = wgpu::Instance::default();
        let backends = wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::PRIMARY);
        instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| {
                log::error!("Failed to create device: {:?}", e);
                e
            })?
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn select_adapter() -> Result<Adapter, DeviceError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            dx12_shader_compiler: wgpu::util::dx12_shader_compiler_from_env().unwrap_or_default(),
            ..Default::default()
        });
        let backends = wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::PRIMARY);
        let adapter = instance
            .enumerate_adapters(backends)
            .max_by_key(|adapter| match adapter.get_info().device_type {
                DeviceType::DiscreteGpu => 5,
                DeviceType::Other => 4,
                DeviceType::IntegratedGpu => 3,
                DeviceType::VirtualGpu => 2,
                DeviceType::Cpu => 1,
            })
            .ok_or(DeviceError::AdapterRequestFailed)?;

        log::info!("Using adapter {:?}", adapter.get_info());
        Ok(adapter)
    }
}

impl Device {
    pub fn create_buffer_init(
        &self,
        desc: &BufferDescriptor,
        queue: &wgpu::Queue,
        contents: &[u8],
    ) -> Result<GPUBuffer, DeviceError> {
        let mut pool = self
            .buffer_pool
            .try_write()
            .map_err(|_| PoolError::ResourceNotAvailable)?;
        let buf = pool.allocate(desc, self);
        queue.write_buffer(&buf.inner, 0, contents);
        Ok(buf)
    }
}