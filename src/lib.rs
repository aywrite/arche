// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The protocol layer: reading a uci command, working out what a `go` may
//! spend, and saying back what the search found.
//!
//! A library as well as a binary, so that a test outside the binary can drive
//! the protocol the way an interface does. The engine is the `arche-core`
//! crate; nothing here knows how to search.

pub mod command;
pub mod instruments;
pub mod params;
mod session;
pub mod time_control;
pub mod uci;

pub use uci::UCI;
