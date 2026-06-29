use crate::api::internal;

pub fn leak(session: internal::Session) -> internal::Session {
    session
}
