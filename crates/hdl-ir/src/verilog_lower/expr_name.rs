//! Name and literal lowering for expressions: signal/constant resolution,
//! dotted struct-field and class-field reads, the unknown-name fallbacks,
//! and the literal forms (string packing, SV time literals). Split out of
//! `expr.rs` to keep both at the module-size standard; the dispatcher and
//! operator/selection forms stay there.

use std::collections::HashMap;

use super::literals::{parse_real_literal, time_literal_fs};
use super::{parse_int_literal, Conn, Lower, TypeKind};
use crate::ir::{Expr, SigId};
use crate::value::{Value, V0, V1};
use hdl_syntax::cst::SyntaxNode;

impl<'a> Lower<'a> {
    pub(super) fn lower_name_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let name = self.text(e);
        if name == "this" || name == "super" {
            return self.this_expr();
        }
        if let Some(v) = env.get(&name).or_else(|| self.gconsts.get(&name)) {
            return Expr::Lit(v.clone());
        }
        // Dotted name: struct field select, class field read, or a
        // hierarchical read of an instance port (`arb.data`).
        if let Some(dot) = name.find('.') {
            if let Some(x) = self.lower_dotted_name(&name, dot) {
                return x;
            }
        }
        let base = name.rsplit('.').next().unwrap_or(name.as_str());
        self.lower_bare_name(e, base, env)
    }

    /// `root.field` resolution. `None` falls through to the bare-name
    /// fallback (the dotted prefix may name something else entirely).
    fn lower_dotted_name(&mut self, name: &str, dot: usize) -> Option<Expr> {
        let root = &name[..dot];
        let field = &name[dot + 1..];
        // Hierarchical read of the auto-instantiated `glbl` primitive.
        if root.eq_ignore_ascii_case("glbl") {
            return Some(Expr::Sig(self.glbl_sig(field)));
        }
        if self.sig_id(root).is_none() {
            if let Some(conns) = self.inst_conns.get(root) {
                if let Some(Conn::Named(_, ex)) = conns
                    .iter()
                    .find(|c| matches!(c, Conn::Named(n, _) if n == field))
                {
                    return Some(ex.clone());
                }
            }
        }
        let id = self.sig_id(root)?;
        let tn = self.sig_types.get(&id).cloned()?;
        let td = self.types.get(&tn).cloned()?;
        match &td.kind {
            TypeKind::Struct(fields) => self.struct_field_select(id, field, fields),
            TypeKind::Class(_) => self.lower_field_read(root, field, id),
            _ => None,
        }
    }

    /// Struct field select: `s.field` → bit range `Sel` from the field's
    /// static (width, lsb) layout.
    fn struct_field_select(
        &self,
        id: SigId,
        field: &str,
        fields: &[(String, usize, usize)],
    ) -> Option<Expr> {
        if let Some((_, fw, lsb)) = fields.iter().find(|(n, _, _)| n == field) {
            return Some(Expr::Sel {
                e: Box::new(Expr::Sig(id)),
                msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + fw - 1) as u64))),
                lsb: Box::new(Expr::Lit(Value::from_u64(32, *lsb as u64))),
            });
        }
        None
    }

    /// The last path segment of a (possibly dotted) name: constant, signal,
    /// or — if neither — the unknown-name fallbacks.
    fn lower_bare_name(
        &mut self,
        e: &SyntaxNode,
        base: &str,
        env: &HashMap<String, Value>,
    ) -> Expr {
        if let Some(v) = env.get(base).or_else(|| self.gconsts.get(base)) {
            return Expr::Lit(v.clone());
        }
        match self.sig_id(base) {
            Some(id) => Expr::Sig(id),
            None => self.lower_unknown_name(e, base),
        }
    }

    /// A bare name that is neither a constant nor a signal.
    fn lower_unknown_name(&mut self, e: &SyntaxNode, base: &str) -> Expr {
        // Bare field of the enclosing class method: `x` ≡ `this.x`.
        if let Some(cn) = self.cur_class_field(base) {
            return Expr::Call {
                name: self
                    .getf_name(&cn, base)
                    .unwrap_or_else(|| format!("$getf::{}::{}", cn, base)),
                args: vec![self.this_expr()],
                shapes: Vec::new(),
            };
        }
        // A bare typedef name used as a value (passing a type to a
        // `parameter type` port, `.rule_t(rule_t)`) — treat as 0.
        if self.types.contains_key(base) {
            return Expr::Lit(Value::from_u64(1, 0));
        }
        // A bare type keyword used as a value (`parameter type t = logic`)
        // means the default type; treat as 0.
        if matches!(
            base,
            "logic"
                | "reg"
                | "wire"
                | "bit"
                | "int"
                | "integer"
                | "longint"
                | "shortint"
                | "byte"
                | "real"
                | "time"
                | "string"
                | "chandle"
                | "signed"
                | "unsigned"
        ) {
            return Expr::Lit(Value::from_u64(1, 0));
        }
        self.err(e.span(), format!("unknown identifier '{}'", base));
        Expr::Lit(Value::x(1))
    }

    pub(super) fn lower_literal(&mut self, l: &SyntaxNode) -> Expr {
        let t = self.text(l);
        if t.starts_with('"') {
            // String literal: first char in the high byte (MSB-first), matching
            // Value::concat part order and string_of's MSB-first reading.
            let inner = t.trim_matches('"');
            let bytes = inner.as_bytes();
            let n = bytes.len().max(1);
            let mut bits = vec![V0; n * 8];
            for (bi, &b) in bytes.iter().enumerate() {
                for j in 0..8 {
                    bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                }
            }
            return Expr::Lit(Value { bits, real: false });
        }
        if let Some(fs) = time_literal_fs(&t) {
            return self.lower_time_literal(fs);
        }
        if let Some(f) = parse_real_literal(&t) {
            return Expr::Real(f);
        }
        match parse_int_literal(&t) {
            Some(v) => Expr::Lit(v),
            None => {
                self.err(l.span(), format!("unsupported literal '{}'", t));
                Expr::Lit(Value::x(32))
            }
        }
    }

    /// SV time literal (`1ns`, `#(2*1ns)`, `#(1 ps)`): the literal's absolute
    /// fs value scaled into the module's time units (SV LRM 5.7.1 scales a
    /// time literal to the module's timescale; hdlsim keeps only the unit
    /// scale, so `#1ns` delays one unit and `#(2*1ns)` composes with the
    /// `#N`-unit delay scaling, and both give the same absolute time).
    fn lower_time_literal(&mut self, fs: u64) -> Expr {
        let units = if self.time_unit_fs == 0 {
            0
        } else {
            ((fs as f64) / (self.time_unit_fs as f64)).round() as u64
        };
        Expr::Lit(
            parse_int_literal(&units.to_string())
                .expect("a u64 unit count always parses as a decimal Verilog literal"),
        )
    }
}
