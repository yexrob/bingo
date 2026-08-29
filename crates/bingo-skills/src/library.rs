//! The one library the command source, the tool and the contributor all read
//! from, so a session sees the same skills whichever way it reaches them.
//!
//! Every look is guarded by a stamp: the directories and files of the last
//! scan, with the length and last-write time each had when it was read. An
//! unchanged tree costs a handful of `stat` calls; a changed one is re-read on
//! the spot, which is what "an edited SKILL.md needs no restart" means.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bingo_sdk::Env;

use crate::skill::Skill;
use crate::{layers, scan};

/// What a path looked like when it was read. The length is there because a
/// clock can be too coarse to notice a rewrite within its own tick.
type Mark = Option<(u64, SystemTime)>;

/// The skills of one working directory, and what would make them stale.
struct Cached {
    stamps: Vec<(PathBuf, Mark)>,
    skills: Arc<[Skill]>,
}

/// Every skill this process can offer, cached per working directory.
#[derive(Debug)]
pub struct Library {
    env: Env,
    cache: Mutex<HashMap<PathBuf, Cached>>,
}

impl std::fmt::Debug for Cached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cached")
            .field("skills", &self.skills.len())
            .finish_non_exhaustive()
    }
}

impl Library {
    pub fn new(env: Env) -> Self {
        Self {
            env,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The skills a session working in `cwd` can run, most important first.
    pub fn skills(&self, cwd: &Path) -> Arc<[Skill]> {
        let mut cache = self.cache.lock().unwrap_or_else(|held| held.into_inner());
        if let Some(cached) = cache.get(cwd).filter(|cached| cached.is_current()) {
            return Arc::clone(&cached.skills);
        }
        let fresh = Cached::of(scan::layers(&layers::dirs(&self.env, cwd)));
        let skills = Arc::clone(&fresh.skills);
        cache.insert(cwd.to_path_buf(), fresh);
        skills
    }
}

impl Cached {
    fn of(scan: scan::Scan) -> Self {
        Self {
            stamps: scan
                .watched
                .iter()
                .map(|path| (path.clone(), mark(path)))
                .collect(),
            skills: scan.skills.into(),
        }
    }

    /// Whether every path this scan read still looks the way it did.
    fn is_current(&self) -> bool {
        self.stamps.iter().all(|(path, seen)| mark(path) == *seen)
    }
}

/// What a path looks like now. A path that is not there marks as nothing, so
/// a directory appearing or vanishing is a change like any other.
fn mark(path: &Path) -> Mark {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Tree;

    fn library(tree: &Tree) -> Library {
        Library::new(Env::rooted(tree.root()))
    }

    #[test]
    fn a_second_look_at_an_unchanged_tree_hands_back_the_same_scan() {
        let tree = Tree::new();
        tree.user_skill("one", "body\n");
        let library = library(&tree);

        let first = library.skills(&tree.cwd());
        let second = library.skills(&tree.cwd());
        assert!(Arc::ptr_eq(&first, &second), "the tree did not change");
    }

    #[test]
    fn an_edited_skill_is_seen_on_the_next_look() {
        let tree = Tree::new();
        tree.user_skill("one", "---\ndescription: before\n---\nbefore\n");
        let library = library(&tree);
        assert_eq!(library.skills(&tree.cwd())[0].description, "before");

        tree.user_skill("one", "---\ndescription: after the edit\n---\nafter\n");
        assert_eq!(
            library.skills(&tree.cwd())[0].description,
            "after the edit",
            "a rewritten SKILL.md needs no restart"
        );
    }

    #[test]
    fn a_new_skill_is_seen_on_the_next_look() {
        let tree = Tree::new();
        tree.user_skill("one", "one\n");
        let library = library(&tree);
        assert_eq!(names(&library.skills(&tree.cwd())), ["one", "guide"]);

        tree.user_skill("two", "two\n");
        assert_eq!(names(&library.skills(&tree.cwd())), ["one", "two", "guide"]);
    }

    #[test]
    fn a_skill_file_written_into_a_directory_that_was_already_there_is_seen() {
        let tree = Tree::new();
        let empty = tree.user_layer().join("late");
        std::fs::create_dir_all(&empty).expect("the directory");
        let library = library(&tree);
        assert_eq!(names(&library.skills(&tree.cwd())), ["guide"]);

        std::fs::write(empty.join(scan::SKILL_FILE), "late\n").expect("the file");
        assert_eq!(names(&library.skills(&tree.cwd())), ["late", "guide"]);
    }

    #[test]
    fn the_person_s_own_skill_overrides_the_project_s_of_that_name() {
        let tree = Tree::new();
        tree.user_skill("deploy", "---\ndescription: the person's\n---\nmine\n");
        let cwd = tree.project_skill(
            "work",
            "deploy",
            "---\ndescription: the project's\n---\np\n",
        );
        let library = library(&tree);

        let skills = library.skills(&cwd);
        assert_eq!(names(&skills), ["deploy", "guide"]);
        assert_eq!(skills[0].description, "the person's");
    }

    #[test]
    fn a_nearer_project_layer_overrides_a_farther_one() {
        let tree = Tree::new();
        tree.project_skill(
            "work",
            "deploy",
            "---\ndescription: the repository's\n---\nr\n",
        );
        let inner = tree.project_skill(
            "work/crate",
            "deploy",
            "---\ndescription: the crate's\n---\nc\n",
        );
        let library = library(&tree);

        assert_eq!(library.skills(&inner)[0].description, "the crate's");
    }

    #[test]
    fn two_working_directories_see_their_own_project_skills() {
        let tree = Tree::new();
        let library = library(&tree);
        let one = tree.project_skill("one", "here", "in one\n");
        let two = tree.project_skill("two", "there", "in two\n");

        assert_eq!(names(&library.skills(&one)), ["here", "guide"]);
        assert_eq!(names(&library.skills(&two)), ["there", "guide"]);
    }

    fn names(skills: &[Skill]) -> Vec<&str> {
        skills.iter().map(|s| s.name.as_str()).collect()
    }
}
