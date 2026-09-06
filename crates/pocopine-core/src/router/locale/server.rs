use std::borrow::Cow;
pub(crate) fn ready() -> bool {
    true
}
pub(crate) fn app_path(path: &str) -> Cow<'_, str> {
    Cow::Borrowed(path)
}
pub(crate) fn href(path: String) -> String {
    path
}
pub(crate) fn can_prefetch(_: &str) -> bool {
    true
}
