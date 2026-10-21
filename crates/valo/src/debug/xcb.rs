use std::fmt::{self, Display, Formatter};

use crate::hooks::xcb::XcbEvent;

pub struct Described<'a>(pub Option<XcbEvent<'a>>);

impl Display for Described<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Some(event) = self.0 else {
            return f.write_str("none");
        };
        match event.parse() {
            Ok(parsed) => write!(f, "{parsed:?} ({})", event.code()),
            Err(e) => write!(f, "unparsable {e:?} ({})", event.code()),
        }
    }
}
