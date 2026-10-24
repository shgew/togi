//! Per-CPU scheduler times from `/proc/stat`. This is the only place that text is parsed.

use togi_machine::{CpuId, CpuTime};

/// Why a `/proc/stat` per-CPU line cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum CpuTimesError {
    #[error("CPU line {line:?} names no CPU number")]
    Cpu { line: String },
    #[error(
        "CPU line {line:?} has fewer than the eight time fields the kernel has written since 2.6.33"
    )]
    Fields { line: String },
    #[error("CPU line {line:?} has a field that is not a tick count")]
    Number {
        line: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("CPU line {line:?} has times whose sum does not fit a 64-bit tick count")]
    Overflow { line: String },
}

/// The cumulative times of every logical CPU, in the order `/proc/stat` lists them. The aggregate `cpu` line and
/// the other counters are skipped. The columns are `user nice system idle iowait irq softirq steal`; guest time is
/// already part of user time. Busy is everything but idle and I/O wait: a CPU waiting for I/O runs nothing.
pub fn cpu_times(stat: &str) -> Result<Vec<CpuTime>, CpuTimesError> {
    stat.lines()
        .filter_map(|line| {
            let (name, fields) = line.split_once(' ')?;
            let digits = name.strip_prefix("cpu")?;
            (!digits.is_empty()).then_some((line, digits, fields))
        })
        .map(|(line, digits, fields)| {
            let cpu = digits
                .parse::<u16>()
                .map(CpuId::from)
                .map_err(|_| CpuTimesError::Cpu {
                    line: line.to_owned(),
                })?;
            let mut ticks = [0_u64; 8];
            let mut columns = fields.split_whitespace();
            for tick in &mut ticks {
                let column = columns.next().ok_or_else(|| CpuTimesError::Fields {
                    line: line.to_owned(),
                })?;
                *tick = column.parse().map_err(|source| CpuTimesError::Number {
                    line: line.to_owned(),
                    source,
                })?;
            }
            let [user, nice, system, idle, iowait, irq, softirq, steal] = ticks;
            let overflow = || CpuTimesError::Overflow {
                line: line.to_owned(),
            };
            let sum = |ticks: &[u64]| {
                ticks
                    .iter()
                    .try_fold(0_u64, |sum, tick| sum.checked_add(*tick))
                    .ok_or_else(overflow)
            };
            let busy = sum(&[user, nice, system, irq, softirq, steal])?;
            Ok(CpuTime {
                cpu,
                busy,
                total: sum(&[busy, idle, iowait])?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test]
    fn busy_excludes_idle_and_io_wait_and_the_aggregate_line_is_skipped() {
        let stat = "cpu  100 0 0 900 0 0 0 0 0 0\n\
                    cpu0 10 1 2 80 4 1 1 1 0 0\n\
                    cpu1 0 0 0 100 0 0 0 0 0 0\n\
                    intr 5\n";
        let times = cpu_times(stat).unwrap();
        assert_eq!(
            times,
            [
                CpuTime {
                    cpu: CpuId::from(0_u16),
                    busy: 16,
                    total: 100,
                },
                CpuTime {
                    cpu: CpuId::from(1_u16),
                    busy: 0,
                    total: 100,
                },
            ]
        );
    }

    #[test_case("cpux 1 2 3 4 5 6 7 8"; "no CPU number")]
    #[test_case("cpu0 1 2 3 4 5 6 7"; "seven fields")]
    #[test_case("cpu0 1 2 3 4 5 6 7 -8"; "negative field")]
    fn a_malformed_cpu_line_is_refused(line: &str) {
        assert!(cpu_times(line).is_err(), "{line}");
    }

    #[test_case("cpu0 18446744073709551615 1 0 0 0 0 0 0"; "busy")]
    #[test_case("cpu0 1 0 0 18446744073709551615 0 0 0 0"; "total through idle")]
    #[test_case("cpu0 0 0 0 0 18446744073709551615 0 0 1"; "total through iowait")]
    fn a_sum_that_overflows_is_refused_not_saturated(line: &str) {
        assert!(matches!(
            cpu_times(line),
            Err(CpuTimesError::Overflow { .. })
        ));
    }
}
