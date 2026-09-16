//! Process memory reporting, isolated per platform.
//!
//! Only Linux is implemented so far; other targets report `None` rather than a
//! guess, so the overlay can say "unavailable" instead of showing a number that
//! means nothing.

/// Resident and virtual memory, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub resident: u64,
    pub virtual_size: u64,
}

impl ProcessMemory {
    pub fn resident_mb(&self) -> f64 {
        self.resident as f64 / (1024.0 * 1024.0)
    }
}

#[cfg(target_os = "linux")]
pub fn process_memory() -> Option<ProcessMemory> {
    // /proc/self/statm reports page counts: total, resident, shared, ...
    let text = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = text.split_whitespace();
    let total_pages: u64 = fields.next()?.parse().ok()?;
    let resident_pages: u64 = fields.next()?.parse().ok()?;
    let page_size = page_size();
    Some(ProcessMemory {
        resident: resident_pages * page_size,
        virtual_size: total_pages * page_size,
    })
}

#[cfg(target_os = "linux")]
fn page_size() -> u64 {
    // SAFETY: sysconf is thread-safe and takes no pointers.
    let v = unsafe {
        libc_sysconf(30 /* _SC_PAGESIZE */)
    };
    if v > 0 {
        v as u64
    } else {
        4096
    }
}

#[cfg(target_os = "linux")]
extern "C" {
    #[link_name = "sysconf"]
    fn libc_sysconf(name: i32) -> i64;
}

#[cfg(not(target_os = "linux"))]
pub fn process_memory() -> Option<ProcessMemory> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_reports_plausible_memory() {
        let m = process_memory().expect("linux should report memory");
        assert!(m.resident > 0);
        assert!(m.virtual_size >= m.resident);
        // A test binary is more than a megabyte and less than a terabyte.
        assert!(m.resident_mb() > 0.5 && m.resident_mb() < 1_000_000.0, "{m:?}");
    }
}
