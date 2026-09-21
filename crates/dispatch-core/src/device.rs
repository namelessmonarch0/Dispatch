//! Machines running a Dispatch daemon.

use serde::{Deserialize, Serialize};

use crate::id::DeviceId;

/// A machine running a daemon, and whether its connection is up.
///
/// The client mints the id: a daemon names itself in `ServerMessage::Hello`
/// but knows nothing of the other machines a client is holding, so identity
/// across the fleet is the client's to assign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Stable identifier.
    pub id: DeviceId,
    /// What the daemon calls itself.
    pub name: String,
    /// Whether its connection is up. A device that goes quiet keeps its rows:
    /// its agents are still running, and hiding them would say otherwise.
    pub reachable: bool,
}

impl Device {
    /// A device named `name`, assumed reachable.
    ///
    /// Reachable because a device is made from a connection that has just
    /// answered; anything else would have failed before reaching here.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            reachable: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_reachable_until_it_is_not() {
        // A device is created from a connection that just answered, so the
        // honest starting point is "reachable".
        let device = Device::new("laptop");

        assert_eq!(device.name, "laptop");
        assert!(device.reachable);
    }
}
