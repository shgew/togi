#[cfg(target_os = "linux")]
mod adapters;
mod clock;
#[cfg(target_os = "linux")]
mod diagnose;
mod files;
#[cfg(target_os = "linux")]
mod host;
mod ranking;
mod support;
mod watchdog;

#[test]
#[cfg(target_os = "linux")]
fn linux_is_a_hardware_platform() {
    assert!(crate::check_platform().is_ok());
}

#[test]
#[cfg(not(target_os = "linux"))]
fn other_platforms_refuse_hardware_runs() {
    let error = crate::check_platform().unwrap_err();
    assert!(matches!(error, crate::Error::Unsupported));
    assert_eq!(
        error.to_string(),
        "hardware runs need Linux: unsupported operation"
    );
}
