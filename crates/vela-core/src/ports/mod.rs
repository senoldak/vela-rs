//! The seams between the core and its collaborators (data providers, market
//! data feeds). Asynchronous methods run on the host services of [`crate::runtime`].

pub mod chart_renderer;
pub mod data_provider;
pub mod market_data;
pub mod scripting_engine;
