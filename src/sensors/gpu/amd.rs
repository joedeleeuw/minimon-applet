use anyhow::{Context, Result, anyhow};
use hex;
use log::{debug, info};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use crate::sensors::gpus::Gpu;
//use log::{debug, warn};

pub struct AmdGpu {
    name: String,
    id: String,
    pci_slot: String,
    device_path: PathBuf,
    usage_path: String,
    vram_used_path: String,
    power_status_path: String,
    temp_input_path: Option<String>,
    vram_total: u64,
    paused: bool,
}

struct AmdPciDevice {
    pci_slot: String,
    path: PathBuf,
    device_id: String,
}

impl AmdGpu {
    pub fn new(name: &str, pci_slot: &str, id: &str, vram_total: u64) -> Self {
        let device_path = PathBuf::from(format!("/sys/bus/pci/devices/{pci_slot}"));
        let base = device_path.to_string_lossy().to_string();
        let temp_input_path = AmdGpu::find_temp_input_path(&device_path);
        Self {
            name: name.to_string(),
            id: id.to_string(),
            pci_slot: pci_slot.to_string(),
            device_path,
            usage_path: format!("{base}/gpu_busy_percent"),
            vram_used_path: format!("{base}/mem_info_vram_used"),
            power_status_path: format!("{base}/power/runtime_status"),
            temp_input_path,
            vram_total,
            paused: false,
        }
    }

    fn driver_name(&self) -> Option<String> {
        fs::read_link(self.device_path.join("driver"))
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
    }

    fn powered_on(&self) -> bool {
        Self::read_file_to_string(&self.power_status_path).map_or(true, |s| s != "suspended")
    }

    fn parse_u32_file(path: &str) -> Option<u32> {
        Self::read_file_to_string(path).ok()?.parse().ok()
    }

    fn parse_u64_file(path: &str) -> Option<u64> {
        Self::read_file_to_string(path).ok()?.parse().ok()
    }

    fn read_file_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
        fs::read_to_string(path).map(|s| s.trim().to_string())
    }

    fn env_truthy(name: &str) -> bool {
        env::var(name)
            .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }

    fn configured_device_ids() -> Option<Vec<String>> {
        let raw = env::var("MINIMON_AMD_DEVICE_IDS").ok()?;
        let ids = raw
            .split(',')
            .map(|id| id.trim().to_ascii_lowercase())
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>();

        (!ids.is_empty()).then_some(ids)
    }

    fn get_amd_pci_devices() -> Vec<AmdPciDevice> {
        debug!("AmdGpu::get_amd_pci_devices().");
        let mut devices = Vec::new();
        let device_filter = Self::configured_device_ids();
        let hide_boot_vga = Self::env_truthy("MINIMON_HIDE_BOOT_VGA");

        if let Ok(entries) = fs::read_dir("/sys/bus/pci/devices") {
            for entry in entries.flatten() {
                let path = entry.path();
                debug!("                    entry {path:?}");
                let Ok(vendor_id) = Self::read_file_to_string(path.join("vendor")) else {
                    continue;
                };
                if !vendor_id.eq_ignore_ascii_case("0x1002") {
                    debug!("                    Not AMD");
                    continue;
                }

                let Ok(device_id) = Self::read_file_to_string(path.join("device")) else {
                    continue;
                };
                if let Some(filter) = &device_filter
                    && !filter.iter().any(|id| id.eq_ignore_ascii_case(&device_id))
                {
                    continue;
                }

                let pci_class = Self::read_file_to_string(path.join("class")).unwrap_or_default();
                if !pci_class.starts_with("0x03") {
                    continue;
                }

                let boot_vga = Self::read_file_to_string(path.join("boot_vga")).unwrap_or_default();
                if hide_boot_vga && boot_vga == "1" {
                    continue;
                }

                if let Some(pci_slot) = path.file_name().and_then(|name| name.to_str()) {
                    devices.push(AmdPciDevice {
                        pci_slot: pci_slot.to_ascii_lowercase(),
                        path,
                        device_id,
                    });
                }
            }
        }
        devices.sort_by(|a, b| a.pci_slot.cmp(&b.pci_slot));
        devices
    }

    fn find_temp_input_path(device_path: &Path) -> Option<String> {
        log::info!("AMD find_temp_input_path({device_path:?})");
        let entries = fs::read_dir(device_path.join("hwmon")).ok()?;

        for entry in entries.flatten() {
            let path = entry.path().join("temp1_input");
            if path.exists() {
                log::info!("    Found temperature file {path:?}");
                return Some(path.to_string_lossy().to_string());
            }
        }

        log::info!("    Couldn't find temp1_input.");
        None
    }

    fn get_vram_total(device_path: &Path, device_id: &str) -> u64 {
        let path = device_path.join("mem_info_vram_total");
        Self::read_file_to_string(path)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| Self::fallback_vram_total(device_id))
            .unwrap_or(0)
    }

    fn fallback_vram_total(device_id: &str) -> Option<u64> {
        match device_id.to_ascii_lowercase().as_str() {
            "0x744c" => Some(24 * 1024 * 1024 * 1024),
            _ => None,
        }
    }

    fn get_lspci_gpu_names() -> Vec<(String, String)> {
        fn clean_gpu_name(model: &str) -> String {
            let (_, truncated) = model.split_once("]:").unwrap_or((model, model));
            let truncated = truncated.split("[1002:").next().unwrap_or(model);
            truncated
                .replace("Corporation", "")
                .replace("[AMD/ATI]", "")
                .replace("compatible controller", "")
                .replace("controller", "")
                .replace("VGA", "")
                .replace("3D", "")
                .replace("Display", "")
                .replace(':', "")
                .replace("  ", " ")
                .replace('[', "(")
                .replace(']', ")")
                .trim()
                .to_string()
        }

        let mut map = Vec::new();
        let output = Command::new("lspci").arg("-nn").output();
        let Ok(output) = output else {
            return map;
        };
        let Ok(stdout) = String::from_utf8(output.stdout) else {
            return map;
        };

        for line in stdout.lines() {
            if (line.contains("VGA") || line.contains("Display") || line.contains("3D"))
                && let Some((slot, rest)) = line.split_once(' ')
            {
                let model = rest.trim();
                let name = clean_gpu_name(model);
                map.push((slot.to_lowercase().to_string(), name));
            }
        }
        map
    }

    fn get_gpu_name(device: &AmdPciDevice, lspci_map: &Vec<(String, String)>) -> String {
        info!("Resolving GPU name for PCI device: {}", device.pci_slot);

        if let Some(name) = AMD_GPU_DEVICE_IDS.get(device.device_id.to_uppercase().as_str()) {
            debug!("Found name in static map: {name}");
            return (*name).to_string();
        }

        for (p, n) in lspci_map {
            if device.pci_slot.contains(p) {
                info!("Found name in lspci_map: {n}");
                return n.clone();
            }
        }
        debug!("No entry in lspci_map for slot: {}", device.pci_slot);

        debug!("Falling back to unknown GPU name");
        "Unknown AMD GPU".to_string()
    }

    fn generate_gpu_id(device_path: &Path) -> Option<String> {
        let pci_address = device_path.canonicalize().ok()?;
        let subsystem_vendor =
            Self::read_file_to_string(device_path.join("subsystem_vendor")).ok()?;
        let subsystem_device =
            Self::read_file_to_string(device_path.join("subsystem_device")).ok()?;

        let mut hasher = Sha256::new();
        hasher.update(pci_address.to_string_lossy().as_bytes());
        hasher.update(subsystem_vendor.as_bytes());
        hasher.update(subsystem_device.as_bytes());

        Some(hex::encode(hasher.finalize()))
    }

    pub fn get_gpus() -> Vec<Gpu> {
        debug!("AmdGpu::get_gpus().");

        let mut gpus = Vec::new();

        let lspci_map = AmdGpu::get_lspci_gpu_names();
        debug!("Available lspci_map entries:");
        for (k, v) in &lspci_map {
            debug!("  {k} -> {v}");
        }

        let devices = AmdGpu::get_amd_pci_devices();

        for device in devices {
            debug!("                    Found AMD PCI device {}", device.pci_slot);
            let vram_total = AmdGpu::get_vram_total(&device.path, &device.device_id);
            debug!("                    total vram {vram_total}");
            if let Some(id) = AmdGpu::generate_gpu_id(&device.path) {
                debug!("                    id {id}");
                let name = AmdGpu::get_gpu_name(&device, &lspci_map);
                debug!("                    name {name}");
                gpus.push(Gpu::new(Box::new(AmdGpu::new(
                    &name,
                    &device.pci_slot,
                    &id,
                    vram_total,
                ))));
            }
        }
        gpus
    }
}

impl super::GpuIf for AmdGpu {
    fn restart(&mut self) {
        debug!("AmdGpu::restart({}).", self.name);
        self.paused = false;
    }

    fn stop(&mut self) {
        debug!("AmdGpu::stop({}).", self.name);
        self.paused = true;
    }

    fn is_active(&self) -> bool {
        !self.paused && self.driver_name().as_deref() == Some("amdgpu")
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn panel_label(&self) -> String {
        let bus = self.pci_slot.split(':').nth(1).unwrap_or(&self.pci_slot);
        format!("G{}", bus.to_ascii_uppercase())
    }

    fn panel_sort_key(&self) -> String {
        self.pci_slot.clone()
    }

    fn inactive_label(&self) -> String {
        if self.paused {
            "---%".to_string()
        } else {
            match self.driver_name().as_deref() {
                None => "AM".to_string(),
                Some("amdgpu") => "---%".to_string(),
                Some(_) => "DRV".to_string(),
            }
        }
    }

    fn vram_total(&self) -> u64 {
        debug!("AmdGpu::vram_total({}) - {}.", self.name, self.vram_total);
        self.vram_total
    }

    fn usage(&self) -> Result<u32> {
        if !self.is_active() {
            return Err(anyhow!("AMD device paused"));
        }
        if !self.powered_on() {
            return Ok(0);
        }
        Ok(Self::parse_u32_file(&self.usage_path).unwrap_or(0))
    }

    fn temperature(&self) -> Result<u32> {
        if !self.powered_on() {
            return Ok(0);
        }

        let path = self
            .temp_input_path
            .as_ref()
            .context("Temperature path not found")?;

        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read temperature from {path}"))?;

        let temp_millidegrees: u32 = contents
            .trim()
            .parse()
            .context("Failed to parse temperature value")?;

        Ok(temp_millidegrees)
    }

    fn vram_used(&self) -> Result<u64> {
        if !self.is_active() {
            return Err(anyhow!("AMD device paused"));
        }
        if !self.powered_on() {
            return Ok(0);
        }
        Ok(Self::parse_u64_file(&self.vram_used_path).unwrap_or(0))
    }
}

impl std::fmt::Debug for AmdGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AmdGpu {{ name: {}, id: {}, paused: {} }}",
            self.name, self.id, self.paused
        )
    }
}

// A hashmap containing AMD graphics card subsystem device IDs and their names
// Keys are the values found in /sys/class/drm/card?/device/subsystem_device
pub static AMD_GPU_DEVICE_IDS: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        let mut m = HashMap::new();

        // Radeon RX 7000 Series
        m.insert("0x744C", "AMD Radeon RX 7700S");
        m.insert("0x73FF", "AMD Radeon RX 7900 XTX");
        m.insert("0x73DF", "AMD Radeon RX 7900 XT");
        m.insert("0x7470", "AMD Radeon RX 7800 XT");
        m.insert("0x7460", "AMD Radeon RX 7700 XT");
        m.insert("0x7420", "AMD Radeon RX 7600");
        m.insert("0x7422", "AMD Radeon RX 7600 XT");

        // Radeon RX 6000 Series
        m.insert("0x73BF", "AMD Radeon RX 6950 XT");
        m.insert("0x73A5", "AMD Radeon RX 6900 XT");
        m.insert("0x73A3", "AMD Radeon RX 6800 XT");
        m.insert("0x73AB", "AMD Radeon RX 6800");
        m.insert("0x73DF", "AMD Radeon RX 6750 XT");
        m.insert("0x73D5", "AMD Radeon RX 6700 XT");
        m.insert("0x73FF", "AMD Radeon RX 6700");
        m.insert("0x73EF", "AMD Radeon RX 6650 XT");
        m.insert("0x73E8", "AMD Radeon RX 6600 XT");
        m.insert("0x73E3", "AMD Radeon RX 6600");
        m.insert("0x7422", "AMD Radeon RX 6500 XT");
        m.insert("0x7424", "AMD Radeon RX 6400");

        // Radeon RX 5000 Series
        m.insert("0x731F", "AMD Radeon RX 5700 XT");
        m.insert("0x7340", "AMD Radeon RX 5700");
        m.insert("0x7341", "AMD Radeon RX 5600 XT");
        m.insert("0x7347", "AMD Radeon RX 5500 XT");

        // Radeon RX Vega Series
        m.insert("0x687F", "AMD Radeon VII");
        m.insert("0x6863", "AMD Radeon RX Vega 64");
        m.insert("0x6867", "AMD Radeon RX Vega 56");

        // Radeon RX 500 Series
        m.insert("0x67DF", "AMD Radeon RX 590");
        m.insert("0x67FF", "AMD Radeon RX 580");
        m.insert("0x67EF", "AMD Radeon RX 570");
        m.insert("0x67E0", "AMD Radeon RX 560");
        m.insert("0x699F", "AMD Radeon RX 550");

        // APUs - Integrated Graphics
        m.insert("0x15BF", "AMD Radeon 780M iGPU");
        m.insert("0x1681", "AMD Radeon 780M iGPU");
        m.insert("0x15E7", "AMD Radeon 760M iGPU");
        m.insert("0x15D8", "AMD Radeon 680M iGPU");
        m.insert("0x1638", "AMD Radeon 660M iGPU");
        m.insert("0x164C", "AMD Radeon 610M iGPU");
        m.insert("0x15DD", "AMD Radeon Vega 8 iGPU");
        m.insert("0x15D8", "AMD Radeon Vega 7 iGPU");

        // Radeon Pro Series
        m.insert("0x73A2", "AMD Radeon Pro W6800");
        m.insert("0x73A3", "AMD Radeon Pro W6600");
        m.insert("0x6867", "AMD Radeon Pro VII");
        m.insert("0x66AF", "AMD Radeon Pro WX 9100");
        m.insert("0x67C4", "AMD Radeon Pro WX 7100");

        m
    });
