macro_rules! find {
    ($events:expr, $variant:ident) => {
        find!($events, $variant, |_| true)
    };
    ($events:expr, $variant:ident, $accept:expr) => {
        $events.iter().find_map(|event| match &event.payload {
            togi_journal::Payload::$variant(payload) => Some(payload).filter($accept),
            _ => None,
        })
    };
}
