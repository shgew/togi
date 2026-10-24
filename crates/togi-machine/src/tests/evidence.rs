use crate::*;
use test_case::test_case;

const RING: KernelFault = KernelFault::GraphicsTimeout {
    driver: GraphicsDriver::Amdgpu,
};

#[test_case(RING => true; "graphics timeout")]
#[test_case(KernelFault::GraphicsReset { driver: GraphicsDriver::Xe } => true; "graphics reset")]
#[test_case(KernelFault::NvidiaXid { code: 79 } => true; "xid")]
#[test_case(KernelFault::MachineCheck {} => false; "machine check")]
#[test_case(KernelFault::Oops {} => false; "oops")]
#[test_case(KernelFault::HungTask {} => false; "hung task")]
fn graphics_faults_are_told_apart(fault: KernelFault) -> bool {
    fault.is_graphics()
}

#[test_case(&KernelTail::Missing {} => false; "missing")]
#[test_case(&KernelTail::Unreadable {} => false; "unreadable")]
#[test_case(&KernelTail::Quiet {} => true; "quiet")]
#[test_case(&KernelTail::Faults { faults: vec![RING], logging_continued: false } => true; "faults")]
fn only_a_tail_that_was_read_means_something_by_its_silence(tail: &KernelTail) -> bool {
    tail.was_read()
}

#[test]
fn only_a_read_tail_has_faults() {
    assert_eq!(KernelTail::Missing {}.faults(), &[]);
    assert_eq!(KernelTail::Unreadable {}.faults(), &[]);
    assert_eq!(KernelTail::Quiet {}.faults(), &[]);
    assert_eq!(
        KernelTail::Faults {
            faults: vec![RING],
            logging_continued: true
        }
        .faults(),
        &[RING]
    );
}

#[test_case(MceCount::Unknown {} => false; "unknown is not a machine check")]
#[test_case(MceCount::Read { corrected: 4, uncorrected: 0 } => false; "corrected only")]
#[test_case(MceCount::Read { corrected: 0, uncorrected: 1 } => true; "uncorrected")]
fn only_a_read_uncorrected_count_is_cpu_evidence(count: MceCount) -> bool {
    count.has_uncorrected()
}

#[test]
fn unknown_evidence_holds_no_machine_check() {
    let unknown = EndEvidence::unknown();
    assert!(!unknown.has_uncorrected_mce());
    assert!(!unknown.tail.was_read());
}

#[test]
fn evidence_round_trips_through_json() {
    let evidence = EndEvidence {
        crashed_mces: MceCount::Read {
            corrected: 1,
            uncorrected: 2,
        },
        successor_mces: MceCount::Unknown {},
        tail: KernelTail::Faults {
            faults: vec![RING, KernelFault::NvidiaXid { code: 13 }],
            logging_continued: true,
        },
        asleep: None,
    };
    let json = serde_json::to_value(&evidence).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "crashed_mces": {"kind": "read", "corrected": 1, "uncorrected": 2},
            "successor_mces": {"kind": "unknown"},
            "tail": {
                "kind": "faults",
                "faults": [
                    {"kind": "graphics_timeout", "driver": "amdgpu"},
                    {"kind": "nvidia_xid", "code": 13},
                ],
                "logging_continued": true,
            },
        })
    );
    assert_eq!(
        serde_json::from_value::<EndEvidence>(json).unwrap(),
        evidence
    );
}

#[test_case(r#"{"kind":"graphics_timeout","driver":"nouveau"}"#; "an unknown driver")]
#[test_case(r#"{"kind":"graphics_timeout"}"#; "a missing driver")]
#[test_case(r#"{"kind":"oops","extra":1}"#; "an unknown field")]
#[test_case(r#"{"kind":"segfault"}"#; "an unknown kind")]
#[test_case(r#"["oops"]"#; "a positional form")]
fn a_malformed_fault_is_refused(text: &str) {
    assert!(serde_json::from_str::<KernelFault>(text).is_err());
}

#[test]
fn a_tail_without_its_continuation_flag_is_refused() {
    assert!(serde_json::from_str::<KernelTail>(r#"{"kind":"faults","faults":[]}"#).is_err());
}
