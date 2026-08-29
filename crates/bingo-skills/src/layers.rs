//! Where skills live, in the order they win. Pure over paths: nothing here
//! asks the filesystem whether a directory exists.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bingo_sdk::Env;

/// The directory a layer keeps its skills in.
const SKILLS: &str = "skills";

/// The directory a project keeps its own configuration in.
const PROJECT: &str = ".bingo";

/// Every directory a skill may live in, most important first: the person's own
/// `<config_dir>/skills`, then `.bingo/skills` at each level from the working
/// directory up to the filesystem root, nearest first.
///
/// ADR-0009 says "from the git common root down to cwd"; walking up from cwd
/// is what happens instead, so a directory outside a repository has skills too
/// and no `git` process is spawned to find out where the root is.
pub fn dirs(env: &Env, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![env.config_dir.join(SKILLS)];
    dirs.extend(project(cwd));
    // The home is often an ancestor of the working directory, and reading one
    // directory twice would be the same skills twice.
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

/// `.bingo/skills` at every level from `cwd` upwards, nearest first: the
/// package a person is working in speaks before the repository around it.
fn project(cwd: &Path) -> Vec<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(PROJECT).join(SKILLS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_person_s_own_skills_come_before_any_project_s() {
        let env = Env::rooted("/home/user");
        let dirs = dirs(&env, Path::new("/work/repo"));
        assert_eq!(dirs[0], PathBuf::from("/home/user/.bingo/skills"));
        assert_eq!(dirs[1], PathBuf::from("/work/repo/.bingo/skills"));
    }

    #[test]
    fn a_nested_directory_speaks_before_the_repository_around_it() {
        let env = Env::rooted("/home/user");
        let dirs = dirs(&env, Path::new("/work/repo/crates/inner"));
        assert_eq!(
            dirs,
            [
                PathBuf::from("/home/user/.bingo/skills"),
                PathBuf::from("/work/repo/crates/inner/.bingo/skills"),
                PathBuf::from("/work/repo/crates/.bingo/skills"),
                PathBuf::from("/work/repo/.bingo/skills"),
                PathBuf::from("/work/.bingo/skills"),
                PathBuf::from("/.bingo/skills"),
            ]
        );
    }

    #[test]
    fn a_home_inside_the_working_tree_is_read_once() {
        let env = Env::rooted("/work");
        let dirs = dirs(&env, Path::new("/work/repo"));
        assert_eq!(
            dirs,
            [
                PathBuf::from("/work/.bingo/skills"),
                PathBuf::from("/work/repo/.bingo/skills"),
                PathBuf::from("/.bingo/skills"),
            ]
        );
    }

    #[test]
    fn the_walk_ends_at_the_filesystem_root() {
        assert_eq!(project(Path::new("/")), [PathBuf::from("/.bingo/skills")]);
    }
}
