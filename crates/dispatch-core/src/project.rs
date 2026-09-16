//! Projects: the directories agents are pointed at.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::ProjectId;

/// Where a project's code lives.
///
/// A project does not have to be a git repository. A plain directory that
/// exists only on one machine is a first-class case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectSource {
    /// A directory with no git repository, or one Dispatch does not track.
    LocalDir,
    /// A git repository, optionally with a known remote.
    GitRepo {
        /// The `origin` remote URL, when there is one.
        remote: Option<String>,
    },
}

/// A directory agents can be spawned against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identifier.
    pub id: ProjectId,
    /// Display name, shown in the sidebar.
    pub name: String,
    /// Absolute path to the project root.
    pub root: PathBuf,
    /// Whether the root is a git repository.
    pub source: ProjectSource,
}

impl Project {
    /// Creates a project rooted at `root`.
    ///
    /// The name defaults to the final path component, which is what the
    /// directory is called on disk and so what the user already recognises.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, source: ProjectSource) -> Self {
        let root = root.into();
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into(),
        );

        Self {
            id: ProjectId::new(),
            name,
            root,
            source,
        }
    }

    /// Overrides the display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_defaults_to_the_directory_name() {
        let project = Project::new("/home/someone/code/dispatch", ProjectSource::LocalDir);
        assert_eq!(project.name, "dispatch");
    }

    #[test]
    fn name_falls_back_to_the_path_when_there_is_no_final_component() {
        let project = Project::new("/", ProjectSource::LocalDir);
        assert_eq!(project.name, "/");
    }

    #[test]
    fn a_project_need_not_be_a_repository() {
        let project = Project::new("/tmp/scratch", ProjectSource::LocalDir);
        assert_eq!(project.source, ProjectSource::LocalDir);
    }

    #[test]
    fn name_can_be_overridden() {
        let project =
            Project::new("/home/someone/code/dispatch", ProjectSource::LocalDir).with_name("work");
        assert_eq!(project.name, "work");
    }
}
