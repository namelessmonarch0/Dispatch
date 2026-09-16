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
}
