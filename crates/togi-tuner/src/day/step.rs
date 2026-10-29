/// Whether the firmware context changed since the committed D.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Firmware {
    Current,
    /// A BIOS, AGESA or microcode change not yet answered by a firmware zero.
    Changed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Freeze {
    Off,
    On,
}

/// Whether the cold boot began with the day paused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pause {
    Running,
    /// Even if the pause ends later in the boot, this boot never promotes.
    Paused,
}

/// How the policy is invoked. Restarts and resumes never earn a new learning step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Invocation {
    SameBoot,
    ColdBoot { pause: Pause },
}
