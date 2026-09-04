//! The opening shot: five seconds, five cuts, one block.
//!
//! `docs/design/tui.md` §11 is the storyboard in words and this is it in
//! code. A ray-marcher walks a world of signed distances ([`sdf`], [`march`]),
//! and the light that lands is spent on a luminance ramp and the theme's own
//! tokens ([`shade`]) — a cell at a time, onto one canvas ([`grid`]).
//!
//! This milestone is the brick and the storyboard. Nothing draws it yet:
//! M70 wires it into the welcome box, with the skip, the short form and the
//! settings.

mod embers;
mod grid;
mod march;
mod mascot;
mod sdf;
mod shade;
