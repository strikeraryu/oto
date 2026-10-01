use std::collections::VecDeque;

/// Shared across processes on the same Mac; unaffected by wall-clock adjustments.
pub fn now_ns() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        #[repr(C)]
        struct Timebase {
            numer: u32,
            denom: u32,
        }
        unsafe extern "C" {
            fn mach_absolute_time() -> u64;
            fn mach_timebase_info(info: *mut Timebase) -> i32;
        }
        static RATIO: OnceLock<(u64, u64)> = OnceLock::new();
        let &(numer, denom) = RATIO.get_or_init(|| {
            let mut info = Timebase { numer: 0, denom: 0 };
            unsafe { mach_timebase_info(&mut info) };
            (u64::from(info.numer), u64::from(info.denom))
        });
        (u128::from(unsafe { mach_absolute_time() }) * u128::from(numer) / u128::from(denom)) as u64
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub offset_ns: i64,
    pub rtt_ns: u64,
}

impl Sample {
    /// t1/t4 are client times, t2/t3 host times. Offset is client minus host.
    pub fn from_exchange(t1: u64, t2: u64, t3: u64, t4: u64) -> Option<Self> {
        if t4 < t1 || t3 < t2 {
            return None;
        }
        let rtt = (t4 - t1).checked_sub(t3 - t2)?;
        if rtt > 500_000_000 {
            return None;
        }
        let offset = ((i128::from(t1) - i128::from(t2)) + (i128::from(t4) - i128::from(t3))) / 2;
        Some(Self {
            offset_ns: i64::try_from(offset).ok()?,
            rtt_ns: rtt,
        })
    }
}

#[derive(Default)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
}
impl ClockSync {
    pub fn observe(&mut self, sample: Sample) -> Sample {
        self.samples.push_back(sample);
        if self.samples.len() > 16 {
            self.samples.pop_front();
        }
        *self.samples.iter().min_by_key(|s| s.rtt_ns).unwrap()
    }
}

pub fn shifted(timestamp: u64, offset: i64) -> u64 {
    (i128::from(timestamp) + i128::from(offset)).clamp(0, i128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clocks_with_different_origins_and_server_processing() {
        let s = Sample::from_exchange(10_000, 2_100, 2_300, 10_400).unwrap();
        assert_eq!(s.offset_ns, 8_000);
        assert_eq!(s.rtt_ns, 200);
        assert_eq!(shifted(2_500, s.offset_ns), 10_500);
    }
    #[test]
    fn rejects_impossible_exchanges() {
        assert!(Sample::from_exchange(10, 20, 100, 30).is_none());
        assert!(Sample::from_exchange(10, 20, 19, 30).is_none());
    }
    #[test]
    fn prefers_low_rtt_and_expires_old_samples() {
        let mut sync = ClockSync::default();
        sync.observe(Sample {
            offset_ns: 10,
            rtt_ns: 1,
        });
        for _ in 0..15 {
            assert_eq!(
                sync.observe(Sample {
                    offset_ns: 99,
                    rtt_ns: 20
                })
                .offset_ns,
                10
            );
        }
        assert_eq!(
            sync.observe(Sample {
                offset_ns: 99,
                rtt_ns: 20
            })
            .offset_ns,
            99
        );
    }
}
