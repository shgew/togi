//! Reads the end of a crashed boot's kernel log. This is the only place the text of those lines is parsed.

use togi_machine::{GraphicsDriver, KernelFault, KernelTail, MAX_FAULTS};

use crate::parse::digits;

/// How many of a boot's last kernel log entries are read.
pub(crate) const TAIL_ENTRIES: &str = "256";

/// The recognized error one kernel log line reports, if any.
fn fault(text: &str) -> Option<KernelFault> {
    // NVIDIA's documented kernel form: NVRM: Xid (0000:03:00): 14, Channel ...
    // https://docs.nvidia.com/deploy/xid-errors/latest/working-with-xid-errors.html
    if let Some(rest) = text.split_once("NVRM: Xid").map(|(_, rest)| rest) {
        let code = rest
            .split_once("):")
            .and_then(|(_, after)| digits(after.trim_start()));
        return code
            .and_then(|(code, _)| code.parse().ok())
            .map(|code| KernelFault::NvidiaXid { code });
    }
    if let Some(fault) = graphics(text) {
        return Some(fault);
    }
    if text.contains("Fatal machine check")
        || text.contains("Machine check:")
        || (text.contains("[Hardware Error]") && text.contains("Uncorrected"))
    {
        return Some(KernelFault::MachineCheck {});
    }
    if text.contains("Oops:")
        || text.contains("kernel BUG at")
        || text.contains("Kernel panic - not syncing")
    {
        return Some(KernelFault::Oops {});
    }
    (text.starts_with("INFO: task ") && text.contains(" blocked for more than "))
        .then_some(KernelFault::HungTask {})
}

/// Graphics driver lines, matched on the text the drivers print (drivers/gpu/drm in Linux) and on the prefix
/// that names the driver: `dev_*` and `drm_*` lines start with the driver and PCI address (`amdgpu 0000:03:00.0: `),
/// and the older `DRM_ERROR` form carries the module tag instead (`[drm:amdgpu_job_timedout [amdgpu]] *ERROR* `).
fn graphics(text: &str) -> Option<KernelFault> {
    if text.starts_with("amdgpu ") || text.contains("[amdgpu]") {
        let driver = GraphicsDriver::Amdgpu;
        // amdgpu_job.c: "ring %s timeout, signaled seq=%u, emitted seq=%u" and "ring %s timeout, but soft recovered".
        if text.contains("ring ") && text.contains(" timeout") {
            return Some(KernelFault::GraphicsTimeout { driver });
        }
        // amdgpu_device.c: "GPU reset begin!", "GPU reset(%d) succeeded!", "GPU mode1 reset"; amdgpu_job.c: "Starting %s
        // ring reset", "Ring %s reset succeeded".
        if text.contains("GPU reset")
            || text.contains("GPU mode1 reset")
            || text.contains("ring reset")
            || (text.contains("Ring ") && text.contains(" reset "))
        {
            return Some(KernelFault::GraphicsReset { driver });
        }
    }
    if text.starts_with("i915 ") {
        let driver = GraphicsDriver::I915;
        // i915_gpu_error.c: "GPU HANG: ecode %d:%x:%08x"; intel_reset.c: "%s context reset due to GPU hang",
        // "reset request timed out", "%s timed out, cancelling all in-flight rendering."
        if text.contains("GPU HANG")
            || text.contains("context reset due to GPU hang")
            || text.contains("reset request timed out")
            || text.contains("timed out, cancelling all in-flight rendering")
        {
            return Some(KernelFault::GraphicsTimeout { driver });
        }
        // intel_reset.c: "GT%u: Resetting chip for %s" and "Resetting %s for %s".
        if text.contains("Resetting ") {
            return Some(KernelFault::GraphicsReset { driver });
        }
    }
    if text.starts_with("xe ") {
        let driver = GraphicsDriver::Xe;
        // xe_guc_submit.c: "GT%u: Timedout job: seqno=%u, lrc_seqno=%u, guc_id=%d, flags=0x%lx in %s [%d]".
        if text.contains("Timedout job") {
            return Some(KernelFault::GraphicsTimeout { driver });
        }
        // xe_gt.c: "reset started", "reset failed (%pe)"; xe_guc_submit.c: "Engine reset: engine_class=%s, ...".
        if text.contains("reset started")
            || text.contains("reset failed")
            || text.contains("Engine reset:")
        {
            return Some(KernelFault::GraphicsReset { driver });
        }
    }
    None
}

/// What the last kernel log entries of a boot say, oldest first. No entries is a missing tail.
pub(crate) fn scan(lines: &[String]) -> KernelTail {
    if lines.is_empty() {
        return KernelTail::Missing {};
    }
    let mut faults: Vec<KernelFault> = Vec::new();
    let mut last = None;
    for (at, line) in lines.iter().enumerate() {
        let Some(found) = fault(line) else {
            continue;
        };
        last = Some(at);
        faults.retain(|kept| *kept != found);
        faults.push(found);
        if faults.len() > MAX_FAULTS {
            faults.remove(0);
        }
    }
    match last {
        None => KernelTail::Quiet {},
        Some(at) => KernelTail::Faults {
            faults,
            logging_continued: at + 1 < lines.len(),
        },
    }
}
