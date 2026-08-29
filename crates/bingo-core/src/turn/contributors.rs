//! Letting the contributors speak: the ordered system blocks and user
//! pieces they add for one placement, and who failed to.

use std::sync::Arc;

use bingo_sdk::{
    ContentPart, ContextContributor, ContextError, ContextPiece, ContextQuery, Placement,
    SystemBlock,
};

#[derive(Default)]
pub struct Gathered {
    pub system: Vec<SystemBlock>,
    /// User pieces with the label of the contributor that owes them.
    pub user: Vec<(String, Vec<ContentPart>)>,
    pub failed: Vec<(String, ContextError)>,
}

/// Every contributor whose placement `want` accepts, in `System{order}`
/// order (others count as order 0), asked with the same query.
pub async fn gather(
    contributors: &[Arc<dyn ContextContributor>],
    want: impl Fn(Placement) -> bool,
    query: ContextQuery<'_>,
) -> Gathered {
    let mut ordered: Vec<&Arc<dyn ContextContributor>> = contributors
        .iter()
        .filter(|c| want(c.placement()))
        .collect();
    ordered.sort_by_key(|c| order_of(c.placement()));
    let mut out = Gathered::default();
    for contributor in ordered {
        match contributor.contribute(query).await {
            Ok(pieces) => absorb(&mut out, contributor.id(), pieces),
            Err(e) => out.failed.push((contributor.id().to_string(), e)),
        }
    }
    out
}

fn order_of(placement: Placement) -> i32 {
    match placement {
        Placement::System { order } => order,
        Placement::RoundStart | Placement::Barrier => 0,
    }
}

fn absorb(out: &mut Gathered, id: &str, pieces: Vec<ContextPiece>) {
    for piece in pieces {
        match piece {
            ContextPiece::System(block) => out.system.push(block),
            ContextPiece::User { parts, .. } => out.user.push((format!("contributor:{id}"), parts)),
        }
    }
}
