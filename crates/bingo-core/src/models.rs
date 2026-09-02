//! The model ruler: what a turn may assume about its model, resolved from
//! owners that never overlap (ADR-0004) — the user's settings, the server's
//! own corrections, the models.dev catalogue and the endpoint's facts. Every
//! measuring site reads the one `ModelCapabilities` this produces. Which ids
//! exist at all is nobody's here but the endpoint's: `served` keeps what each
//! provider answered when it was asked.

pub mod catalog;
pub mod declared;
pub mod learned;
pub mod resolve;
pub mod served;
pub mod vision;

pub use catalog::{ModelCatalog, ModelFacts};
pub use declared::Declared;
pub use learned::{Learned, window_from_overflow};
pub use resolve::{DEFAULT_MAX_TOKENS, max_tokens, resolve};
pub use served::{Offer, Served, ServedModels, Source};
