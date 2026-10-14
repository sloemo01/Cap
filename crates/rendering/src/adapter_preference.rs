//! Reads the per-app GPU preference Windows keeps for the running executable
//! and matches it to an enumerated adapter.
//!
//! Windows lets a user assign an app to a GPU under Settings > System >
//! Display > Graphics. The choice is stored in
//! `HKCU\...\DirectX\UserGpuPreferences` and only surfaces through
//! `IDXGIFactory6::EnumAdapterByGpuPreference`; wgpu enumerates adapters with
//! the plain `IDXGIFactory1` API, so a preference recorded for cap.exe
//! otherwise has no effect on adapter selection.

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserGpuPreference {
    MinimumPower,
    HighPerformance,
}

#[cfg(any(target_os = "windows", test))]
pub fn parse_gpu_preference(raw: &str) -> Option<UserGpuPreference> {
    raw.split(';').find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("GpuPreference") {
            return None;
        }
        match value.trim() {
            "1" => Some(UserGpuPreference::MinimumPower),
            "2" => Some(UserGpuPreference::HighPerformance),
            _ => None,
        }
    })
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{UserGpuPreference, parse_gpu_preference};
    use crate::adapter_luid::{AdapterLuid, detect_adapter_luid_for_wgpu};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
        DXGI_GPU_PREFERENCE_MINIMUM_POWER, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory6,
    };
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{Interface, PCWSTR, w};

    const KEY: PCWSTR = w!("Software\\Microsoft\\DirectX\\UserGpuPreferences");

    fn registered_preference(executable: &Path) -> Option<UserGpuPreference> {
        let name: Vec<u16> = executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut buf = [0u16; 260];
        let mut bytes = (buf.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                KEY,
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                Some(&mut bytes),
            )
        };
        if status.0 != 0 {
            return None;
        }
        let len = (bytes as usize / 2).min(buf.len());
        let value = String::from_utf16_lossy(&buf[..len]);
        parse_gpu_preference(value.trim_end_matches('\0'))
    }

    fn dxgi_luid_for(preference: UserGpuPreference) -> Option<AdapterLuid> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
        let factory6: IDXGIFactory6 = factory.cast().ok()?;
        let gpu_preference = match preference {
            UserGpuPreference::HighPerformance => DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            UserGpuPreference::MinimumPower => DXGI_GPU_PREFERENCE_MINIMUM_POWER,
        };
        let adapter: IDXGIAdapter1 =
            unsafe { factory6.EnumAdapterByGpuPreference(0, gpu_preference) }.ok()?;
        let desc = unsafe { adapter.GetDesc() }.ok()?;
        Some(AdapterLuid {
            low: desc.AdapterLuid.LowPart,
            high: desc.AdapterLuid.HighPart,
        })
    }

    /// Resolves the preference Windows holds for this executable to one of
    /// `instance`'s enumerated adapters. `None` when nothing is stored, it
    /// cannot be resolved, or it points at a non-hardware adapter.
    pub fn preferred_adapter(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
        let executable = std::env::current_exe().ok()?;
        let preference = registered_preference(&executable)?;
        let luid = dxgi_luid_for(preference)?;
        let adapter = instance
            .enumerate_adapters(wgpu::Backends::DX12)
            .into_iter()
            .find(|adapter| detect_adapter_luid_for_wgpu(adapter) == Some(luid))?;
        let info = adapter.get_info();
        if crate::is_software_wgpu_adapter(&info) {
            tracing::warn!(
                adapter_name = info.name,
                "GPU preference resolves to a non-hardware adapter; ignoring it"
            );
            return None;
        }
        tracing::info!(
            adapter_name = info.name,
            adapter_backend = ?info.backend,
            adapter_device_type = ?info.device_type,
            "Using the adapter selected in Windows graphics settings"
        );
        Some(adapter)
    }
}

#[cfg(target_os = "windows")]
pub use imp::preferred_adapter;

#[cfg(not(target_os = "windows"))]
pub fn preferred_adapter(_: &wgpu::Instance) -> Option<wgpu::Adapter> {
    None
}

#[cfg(test)]
mod tests {
    use super::{UserGpuPreference, parse_gpu_preference};

    #[test]
    fn parses_high_performance_entry() {
        assert_eq!(
            parse_gpu_preference("GpuPreference=2;"),
            Some(UserGpuPreference::HighPerformance)
        );
    }

    #[test]
    fn parses_minimum_power_entry() {
        assert_eq!(
            parse_gpu_preference("GpuPreference=1;"),
            Some(UserGpuPreference::MinimumPower)
        );
    }

    #[test]
    fn parses_entry_after_other_flags() {
        assert_eq!(
            parse_gpu_preference("SwapEffectUpgradeEnable=1;GpuPreference=2;"),
            Some(UserGpuPreference::HighPerformance)
        );
    }

    #[test]
    fn parses_entry_with_spacing() {
        assert_eq!(
            parse_gpu_preference(" GpuPreference = 1 ;"),
            Some(UserGpuPreference::MinimumPower)
        );
    }

    #[test]
    fn ignores_missing_or_unknown_entries() {
        assert_eq!(parse_gpu_preference(""), None);
        assert_eq!(parse_gpu_preference("SwapEffectUpgradeEnable=1;"), None);
        assert_eq!(parse_gpu_preference("GpuPreference=3;"), None);
        assert_eq!(parse_gpu_preference("GpuPreference=;"), None);
    }
}
