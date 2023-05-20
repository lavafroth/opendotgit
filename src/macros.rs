/// A macro that expands to a vector of strings.
#[macro_export]
macro_rules! string_vec {
    ($($x:expr),*) => (vec![$($x.to_string()),*]);
}
