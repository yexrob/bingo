//! Reading the layers off disk: one pass over the directories, in the order
//! they win, ending with what the binary ships.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::bundled;
use crate::skill::Skill;

/// The one file a skill directory speaks through.
pub const SKILL_FILE: &str = "SKILL.md";

/// What one scan found, and every path whose change would make it stale.
#[derive(Debug, Default)]
pub struct Scan {
    pub skills: Vec<Skill>,
    /// The layer directories, every skill directory inside them, and every
    /// `SKILL.md` that was read. A layer directory's own timestamp moves when a
    /// skill is added or removed; a skill directory's when its file appears.
    pub watched: Vec<PathBuf>,
}

/// Every layer in order, then the bundled skills. The first skill of a name
/// wins: the person's own overrides the project's, a nearer directory
/// overrides a farther one, and any of them overrides a bundled skill.
pub fn layers(dirs: &[PathBuf]) -> Scan {
    let mut scan = Scan::default();
    for dir in dirs {
        scan.read(dir);
    }
    scan.dedupe();
    scan.append_bundled();
    scan
}

impl Scan {
    /// One layer directory: its skill directories, sorted by name so a listing
    /// does not depend on the order the filesystem happens to hand them over.
    fn read(&mut self, dir: &Path) {
        self.watched.push(dir.to_path_buf());
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut names: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
        names.sort();
        for name in names {
            self.read_skill(&dir.join(&name), &name.to_string_lossy());
        }
    }

    /// One skill directory. `metadata` follows a symlink, so a linked skill
    /// loads and a dangling link is simply not there.
    fn read_skill(&mut self, dir: &Path, name: &str) {
        if !std::fs::metadata(dir).is_ok_and(|meta| meta.is_dir()) {
            return;
        }
        self.watched.push(dir.to_path_buf());
        let file = dir.join(SKILL_FILE);
        let Ok(source) = std::fs::read_to_string(&file) else {
            return;
        };
        self.watched.push(file);
        self.skills
            .push(Skill::parse(name, dir.to_path_buf(), &source));
    }

    /// One skill per name, the first one read.
    fn dedupe(&mut self) {
        let mut seen = HashSet::new();
        self.skills.retain(|skill| seen.insert(skill.name.clone()));
    }

    /// What the binary ships, for every name no directory claimed.
    fn append_bundled(&mut self) {
        let taken: HashSet<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
        let spare: Vec<Skill> = bundled::skills()
            .into_iter()
            .filter(|skill| !taken.contains(skill.name.as_str()))
            .collect();
        self.skills.extend(spare);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Tree;

    #[test]
    fn an_empty_machine_still_has_the_bundled_guide() {
        let scan = layers(&[]);
        assert_eq!(names(&scan), ["guide"]);
    }

    #[test]
    fn a_layer_lists_its_skills_by_name() {
        let tree = Tree::new();
        let layer = tree.dir("layer");
        tree.skill(&layer, "zebra", "---\ndescription: z\n---\nz\n");
        tree.skill(&layer, "alpha", "---\ndescription: a\n---\na\n");
        let scan = layers(&[layer]);
        assert_eq!(names(&scan), ["alpha", "zebra", "guide"]);
    }

    #[test]
    fn the_earlier_layer_wins_a_name() {
        let tree = Tree::new();
        let near = tree.dir("near");
        let far = tree.dir("far");
        tree.skill(&near, "deploy", "---\ndescription: the near one\n---\nn\n");
        tree.skill(&far, "deploy", "---\ndescription: the far one\n---\nf\n");
        let scan = layers(&[near, far]);
        assert_eq!(names(&scan), ["deploy", "guide"]);
        assert_eq!(scan.skills[0].description, "the near one");
    }

    #[test]
    fn a_disk_skill_overrides_the_bundled_one_of_that_name() {
        let tree = Tree::new();
        let layer = tree.dir("layer");
        tree.skill(&layer, "guide", "---\ndescription: mine\n---\nmine\n");
        let scan = layers(&[layer]);
        assert_eq!(names(&scan), ["guide"]);
        assert_eq!(scan.skills[0].description, "mine");
    }

    #[test]
    fn a_directory_without_a_skill_file_is_not_a_skill() {
        let tree = Tree::new();
        let layer = tree.dir("layer");
        tree.dir("layer/empty");
        tree.skill(&layer, "real", "body\n");
        let scan = layers(&[layer]);
        assert_eq!(names(&scan), ["real", "guide"]);
    }

    #[test]
    fn a_layer_that_is_not_there_is_read_as_nothing_and_still_watched() {
        let tree = Tree::new();
        let absent = tree.root().join("nowhere");
        let scan = layers(std::slice::from_ref(&absent));
        assert_eq!(names(&scan), ["guide"]);
        assert!(
            scan.watched.contains(&absent),
            "a layer that appears later must be noticed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_skill_directory_loads_and_a_dangling_one_does_not() {
        let tree = Tree::new();
        let target = tree.skill(
            &tree.dir("elsewhere"),
            "linked",
            "---\ndescription: l\n---\nl\n",
        );
        let layer = tree.dir("layer");
        std::os::unix::fs::symlink(&target, layer.join("linked")).expect("a symlink");
        std::os::unix::fs::symlink(tree.root().join("gone"), layer.join("dangling"))
            .expect("a dangling symlink");

        assert_eq!(names(&layers(&[layer])), ["linked", "guide"]);
    }

    #[test]
    fn every_path_read_is_watched_for_the_next_look() {
        let tree = Tree::new();
        let layer = tree.dir("layer");
        let skill = tree.skill(&layer, "one", "body\n");
        let scan = layers(std::slice::from_ref(&layer));
        assert!(scan.watched.contains(&layer));
        assert!(scan.watched.contains(&skill));
        assert!(scan.watched.contains(&skill.join(SKILL_FILE)));
    }

    fn names(scan: &Scan) -> Vec<&str> {
        scan.skills.iter().map(|s| s.name.as_str()).collect()
    }
}
