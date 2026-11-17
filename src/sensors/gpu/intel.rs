use anyhow::Result;

use crate::sensors::gpus::Gpu;
//use log::{debug, warn};

pub struct IntelGpu {
    pub name: String,
    pub id: String,
}

#[allow(dead_code)]
impl IntelGpu {
    pub fn new(name: String, id: String) -> Self {
        IntelGpu { name, id }
    }
}

impl super::GpuIf for IntelGpu {
    fn restart(&mut self) {
        todo!();
    }

    fn stop(&mut self) {
        todo!();
    }

    fn is_active(&self) -> bool {
        todo!();
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn usage(&self) -> Result<u32> {
        todo!();
    }

    fn temperature(&self) -> Result<u32> {
        todo!();
    }

    fn vram_total(&self) -> u64 {
        todo!();
    }

    fn vram_used(&self) -> Result<u64> {
        todo!();
    }
}

impl IntelGpu {
    pub fn get_gpus() -> Vec<Gpu> {
        Vec::new()
    }

    pub fn is_present() -> bool {
        std::fs::read_dir("/sys/class/drm").is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                std::fs::read_to_string(entry.path().join("device/vendor"))
                    .is_ok_and(|vendor| vendor.trim() == "0x8086")
            })
        })
    }
}
