use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// A regular expression that matches references (e.g. "refs/heads/master").
    pub static ref REFS: Regex = Regex::new(r"(refs(/[\w\-\.\*]+)*/[\w\-\.]+)").unwrap();

    /// A regular expression that matches a commit hash or a reference name.
    pub static ref HEAD: Regex = Regex::new(r"^(ref:.*|[0-9a-f]{40}$)").unwrap();

    /// A regular expression that matches the name of a pack file.
    pub static ref PACK: Regex = Regex::new(r"pack-([a-f0-9]{40})\.pack").unwrap();

    /// A regular expression that matches object hashes (e.g. "1a410efbd13591db07496601ebc7a059dd55cfe9").
    pub static ref OBJECT: Regex = Regex::new(r"(^|\s)([a-f0-9]{40})($|\s)").unwrap();
}
