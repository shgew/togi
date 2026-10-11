//! The scoped fake: readiness follows scripted cgroup membership, identity is valid, and every
//! systemctl call is recorded and answered from a script. Time is tokio's, so tests pause it.
