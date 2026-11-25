//! Circuit model, board profiles, netlist, simulation and rules engine.

pub mod autowire;
pub mod board;
pub mod breadboard;
pub mod circuit;
pub mod component;
pub mod edit;
pub mod engine;
pub mod export;
pub mod flow;
pub mod geometry;
pub mod library;
pub mod netlist;
pub mod perf;
pub mod program;
pub mod project;
pub mod script;
pub mod script_api;
pub mod sim;
pub mod validate;

pub use wirelab_proto as proto;
