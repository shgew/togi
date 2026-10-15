macro_rules! assert_snapshot {
    ($name:literal, $actual:expr) => {{
        let snapshot = include_str!(concat!(
            "snapshots/togi_tool_stats__tests__",
            $name,
            ".snap"
        ));
        let (_, expected) = snapshot.split_once("\n---\n").expect("snapshot metadata");
        assert_eq!($actual.trim_end(), expected.trim_end());
    }};
}

mod evidence;
mod hunts;
mod recovery;
mod requests;
mod runs;
mod scenario;
mod score;
mod support;
