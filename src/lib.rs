// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The protocol layer: reading a uci command, working out what a `go` may
//! spend, and saying back what the search found.
//!
//! A library as well as a binary, because a binary is reachable from nothing.
//! Everything here was written to be driven by an interface and is worth
//! driving from a test the same way, and a test outside the binary had no way
//! in while the binary was all there was.
//!
//! The engine is the `arche-core` crate. Nothing here knows how to search;
//! it knows what to ask for and how to report the answer.

pub mod params;
mod session;
pub mod time_control;
pub mod uci;

pub use uci::UCI;
