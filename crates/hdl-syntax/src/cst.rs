//! Lossless concrete syntax tree (CST).
//!
//! Hand-rolled rowan-style design: immutable green nodes allocated in an arena,
//! with typed red-node views carrying spans. Whitespace and comments are tokens in
//! the tree, so the exact source can always be reconstructed — this is what makes
//! a built-in formatter and refactoring tools possible.
//!
//! Invariants:
//! - Green nodes are immutable and cheaply cloneable (arena index + kind + text).
//! - Every node's span covers exactly its children's spans (no gaps, no overlap).
//! - Reconstruction (`node.text()`) equals the source slice of the root span.
