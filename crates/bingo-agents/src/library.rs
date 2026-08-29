//! Reading the layers off disk: one pass over the directories, in the order
//! they win. A definition is a handful of lines in a handful of files, so
//! every look reads them again — an edited `<name>.md` needs no restart, and
//! there is no cache to invalidate.

use std::collections::HashSet;
use std::path::Path;

use bingo_sdk::Env;

use crate::definition::Definition;
use crate::layers;

/// What a definition file is called.
const SUFFIX: &str = ".md";

/// Every definition a session working in `cwd` can name, most important
/// first. The first definition of a name wins: the nearest project layer
/// overrides the ones above it, and any of them overrides the person's own.
pub fn load(env: &Env, cwd: &Path) -> Vec<Definition> {
    let mut definitions = Vec::new();
    for dir in layers::dirs(env, cwd) {
        read(&dir, &mut definitions);
    }
    let mut seen = HashSet::new();
    definitions.retain(|d| seen.insert(d.name.clone()));
    definitions
}

/// One layer directory's `*.md`, sorted by file name so a listing does not
/// depend on the order the filesystem happens to hand them over.
fn read(dir: &Path, out: &mut Vec<Definition>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
    files.sort();
    for file in files {
        let name = file.to_string_lossy();
        let Some(stem) = name.strip_suffix(SUFFIX) else {
            continue;
        };
        if let Ok(source) = std::fs::read_to_string(dir.join(&file)) {
            out.push(Definition::parse(stem, &source));
        }
    }
}

/// The names on offer, for a message that says what could have been asked for.
pub fn names(definitions: &[Definition]) -> String {
    definitions
        .iter()
        .map(|d| d.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Tree;

    fn loaded(tree: &Tree, cwd: &Path) -> Vec<Definition> {
        load(&Env::rooted(tree.root()), cwd)
    }

    #[test]
    fn an_empty_machine_has_no_definitions() {
        let tree = Tree::new();
        assert!(loaded(&tree, &tree.cwd()).is_empty());
    }

    #[test]
    fn a_layer_lists_its_definitions_by_file_name() {
        let tree = Tree::new();
        tree.user_agent("zebra", "---\ndescription: z\n---\nz\n");
        tree.user_agent("alpha", "---\ndescription: a\n---\na\n");
        assert_eq!(names(&loaded(&tree, &tree.cwd())), "alpha, zebra");
    }

    #[test]
    fn the_project_s_definition_overrides_the_person_s_of_that_name() {
        let tree = Tree::new();
        tree.user_agent("reviewer", "---\ndescription: the person's\n---\nmine\n");
        let cwd = tree.project_agent(
            "work",
            "reviewer",
            "---\ndescription: the project's\n---\np\n",
        );
        let definitions = loaded(&tree, &cwd);
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].description, "the project's");
    }

    #[test]
    fn a_nearer_project_layer_overrides_a_farther_one() {
        let tree = Tree::new();
        tree.project_agent(
            "work",
            "reviewer",
            "---\ndescription: the repository's\n---\nr\n",
        );
        let inner = tree.project_agent(
            "work/crate",
            "reviewer",
            "---\ndescription: the crate's\n---\nc\n",
        );
        assert_eq!(loaded(&tree, &inner)[0].description, "the crate's");
    }

    #[test]
    fn a_file_that_is_not_markdown_is_not_a_definition() {
        let tree = Tree::new();
        tree.write(&tree.user_layer().join("notes.txt"), "not a definition\n");
        tree.user_agent("real", "body\n");
        assert_eq!(names(&loaded(&tree, &tree.cwd())), "real");
    }

    #[test]
    fn an_edited_definition_is_seen_on_the_next_look() {
        let tree = Tree::new();
        tree.user_agent("one", "---\ndescription: before\n---\nb\n");
        assert_eq!(loaded(&tree, &tree.cwd())[0].description, "before");
        tree.user_agent("one", "---\ndescription: after\n---\na\n");
        assert_eq!(loaded(&tree, &tree.cwd())[0].description, "after");
    }
}
