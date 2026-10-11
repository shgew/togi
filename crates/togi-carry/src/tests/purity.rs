use togi_journal::Attribution;
use togi_machine::Condition;

use super::{Archive, Source, before, context, day, pending};

#[test]
fn select_pure_no_io() {
    let mut a = Source::new(day(1), &[0], 1, 1).with_context(&context());
    a.fail(0, -30, Condition::Alone, Attribution::Attributed);
    let mut b = Source::new(day(2), &[0], 2, 1).with_context(&context());
    b.pass(0, -20, Condition::Alone);
    let mut c = Source::new(day(3), &[0], 3, 1).with_context(&context());
    c.pass(0, -10, Condition::Alone);

    let mut archive = Archive::of(&[&a, &b, &c]);
    let discarded = before();
    archive.poison(discarded);
    let chain = [c.id, b.id, a.id, discarded];

    pending(&mut archive, &chain, Some(discarded), &[]);

    for source in [&a, &b, &c] {
        let opened = archive.opened.iter().filter(|&&id| id == source.id).count();
        assert_eq!(opened, 1, "{} opened once", source.id);
        assert_eq!(
            archive.polled(source.id),
            source.events.len(),
            "{} is polled once per event, never again",
            source.id
        );
    }
    assert!(
        !archive.opened.contains(&discarded),
        "the source behind the boundary stays unread"
    );
}
