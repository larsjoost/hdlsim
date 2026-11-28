//! Statement-shape predicates shared by lowering and the kernel.

use super::*;

/// Is this statement the empty body of an interface-only subprogram stub?
/// Declarations without a body (spec-only packages like ieee_proposed's
/// to_* family, forward declarations whose body never landed) lower to
/// `Null` or an empty `Block`; the kernel must not execute them — dispatch
/// treats them as last-resort candidates and built-in names fall through to
/// their kernel implementation.
pub fn is_bodiless(body: &Stmt) -> bool {
    match body {
        Stmt::Null => true,
        Stmt::Block(ss) => ss.is_empty(),
        _ => false,
    }
}
