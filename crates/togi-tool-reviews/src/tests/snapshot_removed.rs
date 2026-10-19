use std::collections::{HashMap, HashSet};

use test_case::test_case;

use crate::fakes::{BASE_SHA, HEAD_SHA, Harness, OLD_BASE_SHA, PREVIOUS_URL};
use crate::process::CommandError;
use crate::rangediff::lost_effects;
use crate::snapshot::tests::{
    ReReview, lossy, manifest, one_patch, parse, patch_status, paths, previous_body, re_review,
    snapshot, snapshot_file,
};

/// The git calls that start with `prefix`.
fn git_calls(h: &Harness, prefix: &str) -> Vec<String> {
    h.fake
        .git_joined()
        .into_iter()
        .filter(|c| c.starts_with(prefix))
        .collect()
}

const X: &str = "diff --git a/x.go b/x.go\n--- a/x.go\n+++ b/x.go\n";

fn x(rest: &str) -> String {
    format!("{X}{rest}")
}

/// The old range had A (x = 1); the new base contains A, as itself or rewritten at another position, and
/// then a lower commit C edits x further. Comparing A with the new base shows x changing, which read by
/// lines looks like the layer dropping A. Provenance comes first: A is inherited, so nothing of it is the
/// layer's removal. A rewrite is proven by a base commit with exactly A's changed lines; the log's first
/// candidate differs only in whitespace and proves nothing.
#[test_case("ancestor")]
#[test_case("rewritten")]
fn snapshot_removed_patch_inherited_with_later_lower_edits(name: &str) {
    let rewritten = format!(
        "\0\n\n{}\0\n\n{}",
        x("@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x =1\n tail\n"),
        x("@@ -9,3 +9,3 @@\n other\n-x = 0\n+x = 1\n end\n")
    );
    let mut r = ReReview::new(
        previous_body(3, OLD_BASE_SHA),
        "1:  aaa1111 < -:  ------- A, in the base\n2:  aaa2222 = 1:  bbb2222 D, another file\n",
        &[
            (
                "aaa1111 aaa1111^",
                &x("@@ -5,3 +5,3 @@\n ctx\n-x = 1\n+x = 0\n tail\n"),
            ),
            (
                "aaa1111^ aaa1111",
                &x("@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x = 1\n tail\n"),
            ),
            // What reading by lines would see: C changed x on top of A, in the base and the head alike.
            ("aaa1111 -- x.go", &x("@@ -6 +6 @@\n-x = 1\n+x = 2\n")),
        ],
    );
    if name == "ancestor" {
        r.ancestors = HashSet::from(["aaa1111".to_owned()]);
    } else {
        r.logs = HashMap::from([("aaa1111".to_owned(), rewritten)]);
    }
    let mut h = r.harness();
    snapshot(&mut h, &["--previous", PREVIOUS_URL, "700"]).expect("snapshot");
    let m = manifest(&h);
    assert_eq!(
        vec![
            patch_status("aaa1111", "", "A, in the base", "removed", false),
            patch_status("aaa2222", "bbb2222", "D, another file", "unchanged", false),
        ],
        m.patches
    );
    assert!(
        m.files.is_empty() && !m.carry_forward,
        "want no removal assigned to this layer: {m:?}"
    );
    assert_eq!("", snapshot_file(&h, "delta.diff"));
    let traced: Vec<String> = git_calls(&h, "git diff")
        .into_iter()
        .filter(|c| c.contains(" -- "))
        .collect();
    assert!(
        traced.is_empty(),
        "git traced the lines of an inherited commit: {traced:?}"
    );
    assert_eq!(
        vec![format!("git merge-base --is-ancestor aaa1111 {BASE_SHA}")],
        git_calls(&h, "git merge-base"),
        "ancestry checks"
    );
    assert_eq!(
        usize::from(name == "rewritten"),
        git_calls(&h, "git log --no-merges").len(),
        "the rewrite check is only asked when the commit is not an ancestor"
    );
}

/// The old range had A (x = "a b"); the new base has a commit that differs from A only in whitespace
/// (x = "ab"), so patch IDs, as git cherry compares them, match. The values differ, so A is not inherited:
/// its loss is traced by lines and its reversal is the layer's removal.
#[test]
fn snapshot_removed_patch_whitespace_rewrite_is_no_proof() {
    let mut h = ReReview {
        logs: HashMap::from([(
            "aaa1111".to_owned(),
            format!(
                "\0\n\n{}",
                x("@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x = \"ab\"\n tail\n")
            ),
        )]),
        ..ReReview::new(
            previous_body(3, OLD_BASE_SHA),
            "1:  aaa1111 < -:  ------- A, x = \"a b\"\n",
            &[
                (
                    "aaa1111 aaa1111^",
                    &x("@@ -5,3 +5,3 @@\n ctx\n-x = \"a b\"\n+x = 0\n tail\n"),
                ),
                (
                    "aaa1111^ aaa1111",
                    &x("@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x = \"a b\"\n tail\n"),
                ),
                (
                    "aaa1111 -- x.go",
                    &x("@@ -6 +6 @@\n-x = \"a b\"\n+x = \"ab\"\n"),
                ),
            ],
        )
    }
    .harness();
    snapshot(&mut h, &["--previous", PREVIOUS_URL, "700"]).expect("snapshot");
    assert_eq!(
        1,
        git_calls(&h, "git log --no-merges").len(),
        "git calls = {:?}, want one rewrite check",
        h.fake.git_joined()
    );
    assert_eq!(
        vec!["x.go"],
        paths(&manifest(&h)),
        "want the traced removal of x.go"
    );
    let delta = snapshot_file(&h, "delta.diff");
    assert!(
        delta.contains("\n-x = \"a b\"\n"),
        "delta.diff = {delta:?}, want the reversal of x = \"a b\""
    );
}

/// A commit that is neither an ancestor of the new base nor in it by patch falls back to the line trace,
/// after both checks said no.
#[test]
fn snapshot_removed_patch_without_provenance_is_traced_by_lines() {
    let mut h = re_review(
        previous_body(3, OLD_BASE_SHA),
        "1:  aaa1111 < -:  ------- dropped\n",
        &[
            ("aaa1111 aaa1111^", &one_patch("d.go", "ctx", "gone")),
            (
                "aaa1111 -- d.go",
                "diff --git a/d.go b/d.go\n--- a/d.go\n+++ b/d.go\n@@ -6 +6 @@\n-old\n+gone\n",
            ),
        ],
    );
    snapshot(&mut h, &["--previous", PREVIOUS_URL, "700"]).expect("snapshot");
    // fetch and range-diff come first, the listing of the range's merges last.
    let kinds: Vec<String> = h.fake.git_calls[2..]
        .iter()
        .map(|args| args[0].clone())
        .collect();
    assert_eq!(
        vec!["merge-base", "diff", "log", "diff", "diff", "log"],
        kinds,
        "git calls after range-diff"
    );
    assert_eq!(
        vec!["d.go"],
        paths(&manifest(&h)),
        "want the traced removal of d.go"
    );
}

#[test_case(&CommandError::new("git merge-base: fatal: Not a valid commit name"); "unknown commit")]
#[test_case(&CommandError::exit("git merge-base", 128); "exit status 128")]
fn snapshot_removed_patch_provenance_failure(error: &CommandError) {
    let mut h = ReReview {
        ancestry_err: HashMap::from([("aaa1111".to_owned(), error.clone())]),
        ..ReReview::new(
            previous_body(3, OLD_BASE_SHA),
            "1:  aaa1111 < -:  ------- dropped\n",
            &[],
        )
    }
    .harness();
    let got =
        snapshot(&mut h, &["--previous", PREVIOUS_URL, "700"]).expect_err("snapshot accepted");
    assert!(
        got.render().contains("merge-base --is-ancestor aaa1111")
            && got.find::<CommandError>() == Some(error),
        "error = {got}, want the ancestry failure surfaced"
    );
    assert!(
        git_calls(&h, "git diff").is_empty(),
        "a failed ancestry check was taken for a no"
    );
}

/// The reversal of a patch that created f with the sole line G.
const CREATED: &str = "diff --git a/f b/f\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-G = 1\n";
/// Since: f exists, emptied.
const EMPTIED: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +0,0 @@\n-G = 1\n";
/// Since: f is gone.
const REMOVED: &str = "diff --git a/f b/f\ndeleted file mode 100644\nindex 2222222..0000000\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-G = 1\n";
/// The reversal of a patch that renamed a to b and added G.
const RENAMED: &str = "diff --git a/b b/a\nsimilarity index 90%\nrename from b\nrename to a\nindex 1111111..2222222 100644\n--- a/b\n+++ b/a\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n";
/// Since, with the rename kept: b without G.
const LOST_G: &str = "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -6 +5,0 @@\n-G = 1\n";
const LOST_G_REVERSAL: &str =
    "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n";
/// The reversal of a patch that created a binary file, and since: it changed.
const BINARY: &str = "diff --git a/p b/p\ndeleted file mode 100644\nindex 1111111..0000000\nBinary files a/p and /dev/null differ\n";
const BINARY_CHANGED: &str =
    "diff --git a/p b/p\nindex 1111111..2222222 100644\nBinary files a/p and b/p differ\n";
const CONTENT_ONLY: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +0,0 @@\n-G = 1\n";

/// Since, with the rename undone: b is gone, a is back.
fn rename_undone() -> String {
    format!(
        "diff --git a/b b/b\ndeleted file mode 100644\nindex 2222222..0000000\n--- a/b\n+++ /dev/null\n@@ -1,10 +0,0 @@\n{}diff --git a/a b/a\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/a\n@@ -0,0 +1,2 @@\n+x\n+y\n",
        "-line\n".repeat(10)
    )
}

#[test_case(CREATED, EMPTIED, EMPTIED, CONTENT_ONLY, "f", "", "modified"; "created file kept but emptied")]
#[test_case(CREATED, REMOVED, EMPTIED, CONTENT_ONLY, "f", "", "modified"; "created file kept in one layer, emptied in both")]
#[test_case(CREATED, REMOVED, REMOVED, CREATED, "f", "", "deleted"; "created file gone in both")]
#[test_case(CREATED, "", "", "", "", "", ""; "created file kept whole")]
#[test_case(RENAMED, LOST_G, LOST_G, LOST_G_REVERSAL, "b", "", "modified"; "rename kept, G lost")]
#[test_case(RENAMED, &rename_undone(), &rename_undone(), RENAMED, "a", "b", "renamed"; "rename undone")]
#[test_case(RENAMED, &rename_undone(), LOST_G, LOST_G_REVERSAL, "b", "", "modified"; "rename undone only in the base")]
#[test_case(RENAMED, "", "", "", "", "", ""; "rename kept, nothing lost")]
#[test_case(BINARY, BINARY_CHANGED, BINARY_CHANGED, "diff --git a/p b/p\nBinary files a/p and b/p differ\n", "p", "", "modified"; "binary created and changed since")]
fn lost_effects_operations(
    reversal: &str,
    base: &str,
    head: &str,
    text: &str,
    path: &str,
    old: &str,
    status: &str,
) {
    let got =
        lost_effects(&parse(reversal)[0], &parse(base), &parse(head)).expect("valid diff numbers");
    let got = got.map(|lost| (lossy(&lost.text), lost.path, lost.old_path, lost.status));
    let want = (!text.is_empty()).then(|| {
        (
            text.to_owned(),
            path.to_owned(),
            old.to_owned(),
            status.to_owned(),
        )
    });
    assert_eq!(want, got);
}

/// The removed patch created the file with one line, the layers keep the file and drop the line. The
/// delta is the content change of a file that still exists, not its deletion.
#[test]
fn snapshot_removed_patch_created_file_kept_empty() {
    let want = "diff --git a/f.go b/f.go\n--- a/f.go\n+++ b/f.go\n@@ -1 +0,0 @@\n-G = 1\n";
    let mut h = re_review(
        previous_body(3, OLD_BASE_SHA),
        "1:  aaa1111 < -:  ------- adds f.go\n",
        &[
            (
                "aaa1111 aaa1111^",
                "diff --git a/f.go b/f.go\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/f.go\n+++ /dev/null\n@@ -1 +0,0 @@\n-G = 1\n",
            ),
            ("aaa1111 -- f.go", want),
        ],
    );
    snapshot(&mut h, &["--previous", PREVIOUS_URL, "700"]).expect("snapshot");
    let m = manifest(&h);
    let f = &m.files;
    assert!(
        f.len() == 1
            && f[0].path == "f.go"
            && f[0].old_path.is_empty()
            && f[0].status == "modified"
            && f[0].removed == 1
            && f[0].added == 0,
        "files = {f:?}, want f.go modified, one line removed"
    );
    assert_eq!(want, snapshot_file(&h, "delta.diff"));
    assert_eq!(want, snapshot_file(&h, &f[0].hunk_file));
}

/// The removed patch renamed a.go to b.go and added G. The layers keep the rename and drop G, so the delta
/// removes G from b.go and does not rename it back.
#[test]
fn snapshot_removed_patch_rename_kept() {
    let lost_g = "diff --git a/b.go b/b.go\n--- a/b.go\n+++ b/b.go\n@@ -6 +5,0 @@\n-G = 1\n";
    let mut h = re_review(
        previous_body(3, OLD_BASE_SHA),
        "1:  aaa1111 < -:  ------- renames a.go and adds G\n",
        &[
            (
                "aaa1111 aaa1111^",
                "diff --git a/b.go b/a.go\nsimilarity index 90%\nrename from b.go\nrename to a.go\nindex 1111111..2222222 100644\n--- a/b.go\n+++ b/a.go\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n",
            ),
            ("aaa1111 base -- b.go a.go", lost_g),
            ("aaa1111 head -- b.go a.go", lost_g),
        ],
    );
    snapshot(&mut h, &["--previous", PREVIOUS_URL, "700"]).expect("snapshot");
    let m = manifest(&h);
    let want =
        "diff --git a/b.go b/b.go\n--- a/b.go\n+++ b/b.go\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n";
    let f = &m.files;
    assert!(
        f.len() == 1
            && f[0].path == "b.go"
            && f[0].old_path.is_empty()
            && f[0].status == "modified"
            && f[0].removed == 1,
        "files = {f:?}, want b.go modified with only G removed, no rename"
    );
    assert_eq!(want, snapshot_file(&h, "delta.diff"));
    assert_eq!(want, snapshot_file(&h, &f[0].hunk_file));
    let want_call = format!(
        "git diff --no-color --no-ext-diff --no-renames -U0 aaa1111 {HEAD_SHA} -- :(literal)b.go :(literal)a.go"
    );
    assert!(
        h.fake.git_joined().contains(&want_call),
        "both names of the renamed file are traced: {:?}",
        h.fake.git_joined()
    );
}
