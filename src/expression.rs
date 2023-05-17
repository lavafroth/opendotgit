use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    pub static ref REFS: Regex = Regex::new(r"(refs(/[a-zA-Z0-9\-\._\*]+)+)").unwrap();
    pub static ref HEAD: Regex = Regex::new(r"^(ref:.*|[0-9a-f]{40}$)").unwrap();
    pub static ref PACK: Regex = Regex::new(r"pack-([a-f0-9]{40})\.pack").unwrap();
    pub static ref OBJECT: Regex = Regex::new(r"(^|\s)([a-f0-9]{40})($|\s)").unwrap();
}
