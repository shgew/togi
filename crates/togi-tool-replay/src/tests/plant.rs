//! A recording a newer build might have written: an event of an unknown kind in the middle of it and events with
//! fields this build does not know. Shared by the unit tests and the binary's, which include this file.

pub(crate) const UNKNOWN_KIND: &str = "future.thing";

pub(crate) fn recording(scenario: togi_watchtest::Scenario) -> String {
    scenario
        .events()
        .unwrap()
        .iter()
        .map(|event| event.encode().unwrap() + "\n")
        .collect()
}

/// Where the planted lines sit, one-based as the reader counts them.
pub(crate) struct Planted {
    /// The line whose event now has an unknown kind, followed by more known events.
    pub kind_line: usize,
    /// The line whose `trial.start` has the unknown field `data.wild`.
    pub wild_line: usize,
    /// The last line, whose event has the unknown envelope field `extra`.
    pub extra_line: usize,
    /// The kind of the last event.
    pub extra_kind: String,
}

impl Planted {
    /// What the journal crate says of the plant, in the part that names the skipped event.
    pub(crate) fn skipped_kind(&self) -> String {
        format!(
            "skipped 1 event of a kind this build does not know ({UNKNOWN_KIND} at line {})",
            self.kind_line
        )
    }

    /// What the journal crate says of the plant, in the part that names the ignored fields.
    pub(crate) fn ignored_fields(&self) -> Vec<String> {
        vec![
            format!("trial.start data.wild (line {})", self.wild_line),
            format!("{} extra (line {})", self.extra_kind, self.extra_line),
        ]
    }
}

fn kind_of(line: &str) -> &str {
    let rest = line.split_once("\"kind\":\"").unwrap().1;
    rest.split_once('"').unwrap().0
}

/// Plants the unknown kind and fields in memory; sequence numbers remain unchanged.
pub(crate) fn plant(text: &mut String) -> Planted {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let find = |kind: &str| {
        lines
            .iter()
            .position(|line| kind_of(line) == kind)
            .unwrap_or_else(|| panic!("no {kind} in the recording"))
    };
    let kind = find("preflight.check");
    let wild = find("trial.start");
    let extra = lines.len() - 1;
    assert!(0 < kind && kind < wild && wild < extra);

    lines[kind] = lines[kind].replacen(
        "\"kind\":\"preflight.check\"",
        &format!("\"kind\":\"{UNKNOWN_KIND}\""),
        1,
    );
    let extra_kind = kind_of(&lines[extra]).to_owned();
    let open = "\"data\":{";
    let at = lines[wild].find(open).unwrap() + open.len();
    let comma = if lines[wild][at..].starts_with('}') {
        ""
    } else {
        ","
    };
    lines[wild].insert_str(at, &format!("\"wild\":1{comma}"));
    lines[extra].insert_str(1, "\"extra\":1,");

    *text = lines.join("\n") + "\n";
    Planted {
        kind_line: kind + 1,
        wild_line: wild + 1,
        extra_line: extra + 1,
        extra_kind,
    }
}
