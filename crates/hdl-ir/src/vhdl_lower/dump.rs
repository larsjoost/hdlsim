//! `--dump-consts` recording for the VHDL lowerer: one helper the
//! declaration-lowering sites call when they bind a constant's final value.
//! Kept in its own submodule so `vhdl_lower.rs` (size-ratcheted) grows only
//! by the one-line recording calls.

use super::Lower;
use crate::value::Value;
use hdl_syntax::source::Span;

impl Lower<'_> {
    /// Emit one `--dump-consts` record for a just-bound constant value; the
    /// scope is the unit currently being lowered (package, entity, or
    /// `entity/arch`). No-op unless `--dump-consts` is armed.
    pub(super) fn dump_const(
        &self,
        name: &str,
        kind: &'static str,
        span: Span,
        status: &'static str,
        v: &Value,
    ) {
        crate::debug::dump_const(
            self.db,
            crate::debug::ConstDump {
                scope: &self.cur_scope,
                name,
                kind,
                span,
                width: v.width(),
                status,
                value: v,
            },
        );
    }
}
