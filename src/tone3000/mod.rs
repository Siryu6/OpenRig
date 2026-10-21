//! TONE3000 (tone3000.com) integration: account link (`auth`) and the
//! library of tones the user browses and downloads models from (`library`),
//! with their pictures (`images`), at a pace TONE3000 accepts (`limiter`).

pub mod auth;
pub mod images;
pub mod library;
pub mod limiter;

pub use auth::*;
