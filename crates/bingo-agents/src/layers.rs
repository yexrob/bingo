//! Where definitions live, in the order they win. Pure over paths: nothing
//! here asks the filesystem whether a directory exists.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bingo_sdk::Env;

/// The directory a layer keeps its definitions in.
const AGENTS: &str = "agents";

/// The directory a project keeps its own configuration in.
const PROJECT: &str = ".bingo";

/// Every directory a definition may live in, most important first:
/// `.bingo/agents` at each level from the working directory up to the
/// filesystem root, nearest first, then the person's own
/// `<config_dir>/agents`.
///
/// The project speaks before the person here, the other way round from skills:
/// a repository that ships a `reviewer` means the reviewer of *this* codebase,
/// and a machine-wide one of that name is the fallback.
pub fn dirs(env: &Env, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = project(cwd);
    dirs.push(env.config_dir.join(AGENTS));
    // The home is often an ancestor of the working directory, and reading one
    // directory twice would be the same definitions twice.
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

/// `.bingo/agents` at every level from `cwd` upwards, nearest first.
fn project(cwd: &Path) -> Vec<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(PROJECT).join(AGENTS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_s_definitions_come_before_the_person_s() {
        let env = Env::rooted("/home/user");
        let dirs = dirs(&env, Path::new("/work/repo"));
        assert_eq!(dirs[0], PathBuf::from("/work/repo/.bingo/agents"));
        assert_eq!(
            dirs.last(),
            Some(&PathBuf::from("/home/user/.bingo/agents"))
        );
    }

    #[test]
    fn a_nested_directory_speaks_before_the_repository_around_it() {
        let env = Env::rooted("/home/user");
        assert_eq!(
            dirs(&env, Path::new("/work/repo/crates/inner")),
            [
                PathBuf::from("/work/repo/crates/inner/.bingo/agents"),
                PathBuf::from("/work/repo/crates/.bingo/agents"),
                PathBuf::from("/work/repo/.bingo/agents"),
                PathBuf::from("/work/.bingo/agents"),
                PathBuf::from("/.bingo/agents"),
                PathBuf::from("/home/user/.bingo/agents"),
            ]
        );
    }

    #[test]
    fn a_home_inside_the_working_tree_is_read_once() {
        let env = Env::rooted("/work");
        assert_eq!(
            dirs(&env, Path::new("/work/repo")),
            [
                PathBuf::from("/work/repo/.bingo/agents"),
                PathBuf::from("/work/.bingo/agents"),
                PathBuf::from("/.bingo/agents"),
            ]
        );
    }

    #[test]
    fn the_walk_ends_at_the_filesystem_root() {
        assert_eq!(project(Path::new("/")), [PathBuf::from("/.bingo/agents")]);
    }
}
