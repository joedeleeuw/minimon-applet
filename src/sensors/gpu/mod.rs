use anyhow::Result;

pub mod amd;
pub mod intel;
pub mod nvidia;

pub trait GpuIf: Send {
    fn name(&self) -> String;
    fn id(&self) -> String;
    fn panel_label(&self) -> String {
        "GPU".to_string()
    }
    fn panel_sort_key(&self) -> String {
        self.id()
    }
    fn inactive_label(&self) -> String {
        "---%".to_string()
    }
    fn usage(&self) -> Result<u32>;

    // Temp in millidegrees Celsius
    fn temperature(&self) -> Result<u32>;
    fn vram_total(&self) -> u64;
    fn vram_used(&self) -> Result<u64>;

    // Stop polling, to allow it to sleep
    fn stop(&mut self);
    // Resume active polling
    fn restart(&mut self);
    // Stopped or active for polling?
    fn is_active(&self) -> bool;
}
