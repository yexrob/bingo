//! The line in the system prompt: what skills exist, and the two ways to
//! reach one. Names and descriptions only — a body is loaded when it is
//! wanted, not before.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, Placement, SystemBlock,
};

use crate::library::Library;
use crate::listing;

/// After the instructions and the project's own files: a skill is a procedure
/// the model may reach for, not the frame it reads the request in.
const ORDER: i32 = 5;

const HEADING: &str = "# Skills";

const PREAMBLE: &str = "\
Procedures written down for this project and this machine. When one of these \
fits what you were asked, call the `Skill` tool with its name before you start \
and follow what comes back. A person reaches the same thing by typing \
`/<name>`.";

/// Lists the skills of the session's working directory.
#[derive(Debug)]
pub struct SkillsContributor {
    library: Arc<Library>,
}

impl SkillsContributor {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl ContextContributor for SkillsContributor {
    fn id(&self) -> &str {
        "context:skills"
    }

    fn placement(&self) -> Placement {
        Placement::System { order: ORDER }
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let skills = self.library.skills(query.cwd);
        if skills.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![ContextPiece::System(SystemBlock {
            text: format!("{HEADING}\n\n{PREAMBLE}\n\n{}", listing::lines(&skills)),
            cache: true,
        })])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Tree, asked};

    async fn block(tree: &Tree) -> Vec<ContextPiece> {
        let library = Arc::new(Library::new(bingo_sdk::Env::rooted(tree.root())));
        let asked = asked(&tree.cwd());
        SkillsContributor::new(library)
            .contribute(asked.query())
            .await
            .expect("skills never fail a turn")
    }

    fn text(pieces: &[ContextPiece]) -> String {
        match &pieces[0] {
            ContextPiece::System(block) => block.text.clone(),
            ContextPiece::User { .. } => panic!("skills are a system block"),
        }
    }

    #[test]
    fn it_comes_after_the_project_s_own_instructions() {
        let library = Arc::new(Library::new(bingo_sdk::Env::rooted("/tmp/nowhere")));
        let contributor = SkillsContributor::new(library);
        assert_eq!(contributor.id(), "context:skills");
        assert_eq!(contributor.placement(), Placement::System { order: 5 });
    }

    #[tokio::test]
    async fn every_skill_is_listed_once_with_what_it_is_for() {
        let tree = Tree::new();
        tree.user_skill("deploy", "---\ndescription: Ship the build\n---\nbody\n");
        tree.project_skill(
            "work",
            "review",
            "---\ndescription: Read a diff\n---\nbody\n",
        );

        let pieces = block(&tree).await;
        assert_eq!(pieces.len(), 1);
        insta::assert_snapshot!(text(&pieces));
    }

    #[tokio::test]
    async fn the_block_is_cacheable_because_it_changes_only_when_the_disk_does() {
        let tree = Tree::new();
        let pieces = block(&tree).await;
        let ContextPiece::System(block) = &pieces[0] else {
            panic!("a system block");
        };
        assert!(block.cache);
    }
}
