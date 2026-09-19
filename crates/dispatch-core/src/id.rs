//! Stable identifiers for the things Dispatch tracks.
//!
//! Each is a newtype over a UUID rather than an index, because a later slice
//! moves this state into a daemon and sends it across the wire. Indices would
//! be invalidated by any reordering; these survive it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Generates an identifier newtype with a consistent surface.
macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new, randomly generated identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Ok(Self(text.parse()?))
            }
        }
    };
}

id_type! {
    /// Identifies a machine running a Dispatch daemon.
    ///
    /// Unused in Slice 1, which is single-machine. Reserved so the federation
    /// slice does not have to reshape the types below.
    DeviceId
}

id_type! {
    /// Identifies a project: a directory, with or without a git repository.
    ProjectId
}

id_type! {
    /// Identifies one agent pane.
    PaneId
}

id_type! {
    /// Identifies one delegation request.
    ///
    /// Separate from [`PaneId`] because a request has a life before a pane
    /// does: it can be refused or denied and never become one.
    RequestId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_unique() {
        assert_ne!(PaneId::new(), PaneId::new());
        assert_ne!(ProjectId::new(), ProjectId::new());
        assert_ne!(DeviceId::new(), DeviceId::new());
    }

    #[test]
    fn identifiers_round_trip_through_their_uuid() {
        let id = PaneId::new();
        assert_eq!(id.as_uuid(), id.as_uuid());
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }

    #[test]
    fn an_id_survives_being_written_out_and_read_back() {
        // A pane's id reaches a subagent through the environment, as text.
        let id = PaneId::new();
        let parsed: PaneId = id.to_string().parse().expect("its own output parses");

        assert_eq!(parsed, id);
    }

    #[test]
    fn text_that_is_not_an_id_is_refused() {
        assert!("not-a-uuid".parse::<PaneId>().is_err());
        assert!("".parse::<PaneId>().is_err());
    }
}
