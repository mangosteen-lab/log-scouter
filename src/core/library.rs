//! Where a library item came from.
//!
//! Schemas, filters and saved searches all arrive from the same stack of tiers -- the
//! project's own `.logscouter/<kind>`, the user's `~/.log-scouter/<kind>`, each enabled hub,
//! and (for schemas) the formats bundled into the binary. Once loaded they are just items,
//! and two of them with the same name are indistinguishable. `Origin` is what a picker uses
//! to tell the user which `Spring Boot` they are about to apply.

use serde::{Deserialize, Serialize};

/// The tier an item was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// This project's `.logscouter/<kind>` folder.
    Project,
    /// The shared `~/.log-scouter/<kind>` folder.
    User,
    /// A hub, by its local name.
    Hub(String),
    /// A third-party format compiled into the binary, shadowable by any tier above.
    Bundled,
    /// A structural schema the app cannot work without (`Generic line`, the bracketed
    /// default). Distinct from `Bundled`: these are not library files and exist in every
    /// project whatever the libraries hold.
    Builtin,
}

impl Origin {
    /// The tag a picker or a row shows.
    pub fn label(&self) -> String {
        match self {
            Origin::Project => "[Project]".to_string(),
            Origin::User => "[User]".to_string(),
            Origin::Hub(name) => format!("[Hub {name}]"),
            Origin::Bundled => "[Bundled]".to_string(),
            Origin::Builtin => "[Built-in]".to_string(),
        }
    }
}

/// The tag for an optional origin: hand-made items have none, and get no tag rather than a
/// misleading one.
pub fn origin_label(origin: Option<&Origin>) -> String {
    origin.map(Origin::label).unwrap_or_default()
}

/// An item plus the tier it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryItem<T> {
    pub item: T,
    pub origin: Origin,
}

impl<T> LibraryItem<T> {
    pub fn new(item: T, origin: Origin) -> Self {
        Self { item, origin }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_labelled_for_the_picker() {
        assert_eq!(Origin::Project.label(), "[Project]");
        assert_eq!(Origin::User.label(), "[User]");
        assert_eq!(Origin::Bundled.label(), "[Bundled]");
        assert_eq!(Origin::Hub("acme".into()).label(), "[Hub acme]");
        // The official hub is named like any other, so it reads `[Hub official]`.
        assert_eq!(Origin::Hub("official".into()).label(), "[Hub official]");
    }
}
