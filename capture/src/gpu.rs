//! Making sense of the GPU numbers Windows and the driver report.

use culpritcore::model::GpuThrottle;
use std::collections::HashMap;

/// How performance counter instances name a GPU adapter: "luid_0x00000000_0x0000C2A6".
pub fn luid_tag(high: i32, low: u32) -> String {
    format!("luid_0x{:08X}_0x{low:08X}", high as u32)
}

/// The adapter part of a counter instance name, e.g. from
/// "pid_1234_luid_0x00000000_0x0000C2A6_phys_0_eng_0_engtype_3D".
fn adapter_of(instance: &str) -> Option<&str> {
    let start = instance.find("luid_")?;
    instance.get(start..start + 26)
}

/// The number for `adapter` when it has one, else the largest of any adapter.
fn pick(per_adapter: HashMap<String, f64>, adapter: Option<&str>) -> Option<f64> {
    let wanted = adapter.and_then(|a| per_adapter.iter().find(|(k, _)| k.eq_ignore_ascii_case(a)).map(|(_, v)| *v));
    wanted.or_else(|| per_adapter.values().copied().reduce(f64::max))
}

/// 3D engine use, 0 to 100, from the `\GPU Engine(*)\Utilization Percentage` instances,
/// which are one per process and engine: like Task Manager, each engine's use is summed
/// over processes and the busiest 3D engine counts.
pub fn engine_3d_busy<'a>(items: impl IntoIterator<Item = (&'a str, f64)>, adapter: Option<&str>) -> Option<f32> {
    let mut engines: HashMap<String, f64> = HashMap::new();
    for (name, value) in items {
        let Some(at) = name.find("luid_") else { continue };
        let Some((engine, kind)) = name[at..].split_once("_engtype_") else { continue };
        if kind.eq_ignore_ascii_case("3D") && value.is_finite() {
            *engines.entry(engine.to_ascii_lowercase()).or_default() += value;
        }
    }
    let mut per_adapter: HashMap<String, f64> = HashMap::new();
    for (engine, busy) in engines {
        let a = per_adapter.entry(adapter_of(&engine).unwrap_or(&engine).to_string()).or_default();
        *a = a.max(busy);
    }
    pick(per_adapter, adapter).map(|v| v.clamp(0.0, 100.0) as f32)
}

/// Dedicated GPU memory in use, in MB, from the `\GPU Adapter Memory(*)\Dedicated Usage`
/// instances (bytes, one per adapter and physical GPU).
pub fn adapter_memory_mb<'a>(items: impl IntoIterator<Item = (&'a str, f64)>, adapter: Option<&str>) -> Option<f32> {
    let mut per_adapter: HashMap<String, f64> = HashMap::new();
    for (name, bytes) in items {
        if let Some(a) = adapter_of(name).filter(|_| bytes.is_finite()) {
            *per_adapter.entry(a.to_ascii_lowercase()).or_default() += bytes;
        }
    }
    pick(per_adapter, adapter).map(|b| (b / 1_048_576.0) as f32)
}

/// What NVIDIA's driver says is holding the GPU's clock down (NVML's clock event reasons).
/// Hitting the normal power limit isn't counted: boosting up to it is how GPUs work.
pub fn nvml_throttle(reasons: u64) -> GpuThrottle {
    const HW_SLOWDOWN: u64 = 0x8;
    const SW_THERMAL: u64 = 0x20;
    const HW_THERMAL: u64 = 0x40;
    const HW_POWER_BRAKE: u64 = 0x80;
    GpuThrottle {
        thermal: reasons & (SW_THERMAL | HW_THERMAL) != 0,
        power_brake: reasons & HW_POWER_BRAKE != 0,
        hw_slowdown: reasons & HW_SLOWDOWN != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_d_use_per_adapter() {
        let dgpu = luid_tag(0, 0xC2A6);
        assert_eq!(dgpu, "luid_0x00000000_0x0000C2A6");
        let items = [
            ("pid_100_luid_0x00000000_0x0000C2A6_phys_0_eng_0_engtype_3D", 70.0),
            ("pid_200_luid_0x00000000_0x0000c2a6_phys_0_eng_0_engtype_3D", 15.0),
            ("pid_100_luid_0x00000000_0x0000C2A6_phys_0_eng_3_engtype_Copy", 90.0),
            ("pid_300_luid_0x00000000_0x0000C2A6_phys_0_eng_5_engtype_3D", 30.0),
            ("pid_100_luid_0x00000000_0x00001111_phys_0_eng_0_engtype_3D", 95.0),
            ("junk", 50.0),
        ];
        assert_eq!(engine_3d_busy(items, Some(&dgpu)), Some(85.0));
        assert_eq!(engine_3d_busy(items, None), Some(95.0));
        assert_eq!(engine_3d_busy(items, Some("luid_0x00000000_0x00009999")), Some(95.0));
        assert_eq!(engine_3d_busy([], None), None);
        assert_eq!(
            engine_3d_busy([("pid_1_luid_0x00000000_0x00000001_phys_0_eng_0_engtype_3D", 60.0); 2], None),
            Some(100.0)
        );
    }

    #[test]
    fn memory_per_adapter() {
        let items = [
            ("luid_0x00000000_0x0000C2A6_phys_0", 2_147_483_648.0),
            ("luid_0x00000000_0x00001111_phys_0", 104_857_600.0),
        ];
        assert_eq!(adapter_memory_mb(items, Some("luid_0x00000000_0x00001111")), Some(100.0));
        assert_eq!(adapter_memory_mb(items, None), Some(2048.0));
    }

    #[test]
    fn throttle_reasons() {
        assert!(!nvml_throttle(0x1 | 0x4).any());
        let t = nvml_throttle(0x40 | 0x80);
        assert!(t.thermal && t.power_brake && !t.hw_slowdown);
        assert!(nvml_throttle(0x8).hw_slowdown);
    }
}
