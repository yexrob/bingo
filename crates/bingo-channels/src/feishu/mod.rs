//! The Feishu adapter (ADR-0016 §6): the first real platform.
//!
//! Every mechanism it hands over is the one the platform actually has, which
//! is not the obvious one. Editing is **not** `PUT /im/v1/messages/:id` — that
//! is capped at twenty edits per message for the life of the message, which
//! works in a demo and dies in a week. It is CardKit: a card entity, sent by
//! id, then updated with the whole text under a sequence that only ever goes
//! up. Buttons are a card of their own, because callbacks do not fire while a
//! card is streaming.
//!
//! The credentials never come from the settings file: the app id is public and
//! lives there, the secret comes from the environment.

pub mod api;
pub mod bootstrap;
pub mod chunks;
pub mod event;
pub mod frame;
pub mod posted;
pub mod token;
pub mod ws;
