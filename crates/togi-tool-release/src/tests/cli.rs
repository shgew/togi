use test_case::test_case;

use super::*;

fn parsed(args: &[&str]) -> Parsed {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    parse(&args)
}

#[test_case(&[], Command::Preview ; "no flags")]
#[test_case(&["-commit"], Command::Commit ; "commit")]
#[test_case(&["--commit"], Command::Commit ; "double dash")]
#[test_case(&["-commit=true"], Command::Commit ; "explicit true")]
#[test_case(&["-commit=1", "-commit=F"], Command::Preview ; "last value wins")]
#[test_case(&["-publish"], Command::Publish ; "publish")]
#[test_case(&["-check", "changes"], Command::Check(PathBuf::from("changes")) ; "check")]
#[test_case(&["-check=changes"], Command::Check(PathBuf::from("changes")) ; "check with equals")]
#[test_case(&["--check", "changes", "--"], Command::Check(PathBuf::from("changes")) ; "terminator")]
#[test_case(&["-check="], Command::Preview ; "empty check")]
fn runs(args: &[&str], want: Command) {
    assert_eq!(parsed(args), Parsed::Run(want));
}

#[test_case(&["-h"] ; "short")]
#[test_case(&["-help"] ; "long")]
#[test_case(&["--help", "-unknown"] ; "before other flags")]
fn helps(args: &[&str]) {
    assert_eq!(parsed(args), Parsed::Help);
}

#[test_case(&["-dry-run"], Some("flag provided but not defined: -dry-run") ; "unknown flag")]
#[test_case(&["-check"], Some("flag needs an argument: -check") ; "check without directory")]
#[test_case(&["-commit=yes"], Some("invalid boolean value \"yes\" for -commit: parse error") ; "bad boolean")]
#[test_case(&["---commit"], Some("bad flag syntax: ---commit") ; "three dashes")]
#[test_case(&["-=x"], Some("bad flag syntax: -=x") ; "no name")]
#[test_case(&["changes"], None ; "positional argument")]
#[test_case(&["-commit", "--", "x"], None ; "argument after terminator")]
#[test_case(&["-commit", "-publish"], None ; "commit and publish")]
#[test_case(&["-check", "changes", "-commit"], None ; "check and commit")]
fn refuses(args: &[&str], message: Option<&str>) {
    assert_eq!(parsed(args), Parsed::Usage(message.map(ToOwned::to_owned)));
}
