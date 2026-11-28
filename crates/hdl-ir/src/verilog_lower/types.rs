//! Verilog type lowering: `struct packed` field layout, and `typedef`
//! resolution into the type table — enum (labels become constants),
//! struct, or plain base-type forms. Split out of `verilog_lower.rs` to
//! keep it at its standards baseline.

use std::collections::HashMap;

use super::{const_eval, is_expr_node, real_children, type_kw_width, Lower, TypeDef, TypeKind};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    /// Parse a `struct packed { ... }` node into a struct `TypeDef` (fields with
    /// bit offsets, first field at the MSB).
    pub(super) fn struct_type_def(
        &mut self,
        st: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> TypeDef {
        let sk = real_children(st);
        let mut fields: Vec<(String, usize)> = Vec::new();
        let mut i = 0;
        while i < sk.len() {
            let mut width = 1usize;
            if sk[i].kind() == SyntaxKind::Ident {
                let t = self.text(&sk[i]);
                width = type_kw_width(&t)
                    .or_else(|| self.types.get(&t).map(|td| td.width))
                    .unwrap_or(1);
                i += 1;
                if sk.get(i).is_some_and(|k| k.kind() == SyntaxKind::Range) {
                    width = self.range_width(&sk[i], env);
                    i += 1;
                }
            }
            while i < sk.len() && sk[i].kind() == SyntaxKind::Name {
                fields.push((self.text(&sk[i]), width));
                i += 1;
                if sk.get(i).is_some_and(|k| k.kind() == SyntaxKind::Range) {
                    i += 1; // array field dimension
                }
            }
            // skip `;`
            i += 1;
        }
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut lsb = total;
        let mut with_off: Vec<(String, usize, usize)> = Vec::new();
        for (n, w) in &fields {
            lsb -= w;
            with_off.push((n.clone(), *w, lsb));
        }
        let signed = sk
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "signed");
        TypeDef {
            width: total.max(1),
            signed,
            real: false,
            kind: TypeKind::Struct(with_off),
        }
    }

    /// Lower a `typedef` into the type table; enum labels also become
    /// constants. `pkg` prefixes names when the typedef lives in a package.
    pub(super) fn lower_typedef(
        &mut self,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
        pkg: Option<&str>,
    ) {
        let kids = real_children(item);
        let name = kids
            .iter()
            .rev()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k));
        let Some(name) = name else { return };
        let key = match pkg {
            Some(p) => format!("{}::{}", p, name),
            None => name.clone(),
        };
        let body = kids.iter().find(|k| {
            matches!(k.kind(), SyntaxKind::EnumType | SyntaxKind::StructType)
                || (k.kind() == SyntaxKind::Ident && self.types.contains_key(&self.text(k)))
                || matches!(k.kind(), SyntaxKind::Range)
        });
        // Determine the type body: EnumType / StructType node, or a base type
        // ident (+ optional range) for `typedef logic [7:0] word_t;`.
        let enum_node = kids.iter().find(|k| k.kind() == SyntaxKind::EnumType);
        let struct_node = kids.iter().find(|k| k.kind() == SyntaxKind::StructType);
        if let Some(en) = enum_node {
            self.lower_typedef_enum(key, en, env, pkg);
            return;
        }
        if let Some(st) = struct_node {
            let td = self.struct_type_def(st, env);
            self.types.insert(key, td);
            return;
        }
        let _ = body;
        self.lower_typedef_plain(key, &kids, env);
    }

    /// The enum branch: width from the optional base range, labels with
    /// `= expr` overrides, gconsts (plain + pkg-qualified), and the
    /// `TypeDef` carrying the label/value list.
    fn lower_typedef_enum(
        &mut self,
        key: String,
        en: &SyntaxNode,
        env: &HashMap<String, Value>,
        pkg: Option<&str>,
    ) {
        let ek = real_children(en);
        // Optional base type + range.
        let mut width = 32usize;
        if let Some(r) = ek.iter().find(|k| k.kind() == SyntaxKind::Range) {
            width = self.range_width(r, env);
        } else if ek.iter().any(|k| k.kind() == SyntaxKind::Name) {
            if let Some(r) = ek.iter().find(|k| k.kind() == SyntaxKind::Range) {
                width = self.range_width(r, env);
            }
        }
        let vals = self.enum_labels(&ek, env);
        for (label, v) in &vals {
            let lv = Value::from_u64(width, *v);
            self.gconsts.insert(label.clone(), lv.clone());
            if let Some(p) = pkg {
                self.gconsts.insert(format!("{}::{}", p, label), lv);
            }
        }
        self.types.insert(
            key,
            TypeDef {
                width,
                signed: false,
                real: false,
                kind: TypeKind::Enum(vals),
            },
        );
    }

    /// Collect the enum labels in declaration order, honoring `= expr`
    /// explicit values; skips the optional base-type name.
    fn enum_labels(
        &mut self,
        ek: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Vec<(String, u64)> {
        let mut vals: Vec<(String, u64)> = Vec::new();
        let mut next = 0u64;
        for k in ek {
            if k.kind() == SyntaxKind::Name && self.text(k) != "enum" {
                // Could be the base type name; enum labels are Names too.
                // Distinguish: labels are followed by `=`/`,`/`}`; the base
                // type name is the first Name and is a type keyword or typedef.
                let t = self.text(k);
                if vals.is_empty() && (type_kw_width(&t).is_some() || self.types.contains_key(&t)) {
                    continue;
                }
                let mut v = next;
                // Look for `= expr` right after this Name: the child after
                // the label must be the `=` punct, then the value expression.
                if let Some(pos) = ek.iter().position(|x| std::ptr::eq(x, k)) {
                    let has_eq = ek.get(pos + 1).is_some_and(|c| {
                        c.kind() == SyntaxKind::Punct && self.db.span_text(c.span()).trim() == "="
                    });
                    if has_eq {
                        if let Some(e) = ek[pos + 2..].iter().find(|e| is_expr_node(e.kind())) {
                            if let Some(cv) = const_eval(&self.lower_expr(e, env), env) {
                                if let Some(n) = cv.to_u64() {
                                    v = n;
                                }
                            }
                        }
                    }
                }
                next = v + 1;
                vals.push((t, v));
            }
        }
        vals
    }

    /// The plain branch: base type keyword or existing typedef, optional
    /// range, `signed`/`real` modifiers.
    fn lower_typedef_plain(
        &mut self,
        key: String,
        kids: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) {
        // Plain typedef: base type keyword or existing typedef, optional range.
        let mut width = 1usize;
        let mut signed = false;
        let mut real = false;
        if let Some(r) = kids.iter().find(|k| k.kind() == SyntaxKind::Range) {
            width = self.range_width(r, env);
        }
        for k in kids {
            if k.kind() == SyntaxKind::Ident {
                let t = self.text(k);
                match t.as_str() {
                    "signed" => signed = true,
                    "real" => {
                        real = true;
                        width = 64;
                    }
                    "typedef" => {}
                    _ => {
                        if width == 1 {
                            if let Some(w) = type_kw_width(&t) {
                                width = w;
                            } else if let Some(td) = self.types.get(&t) {
                                width = td.width;
                                signed = td.signed;
                                real = td.real;
                            }
                        }
                    }
                }
            }
        }
        self.types.insert(
            key,
            TypeDef {
                width,
                signed,
                real,
                kind: TypeKind::Plain,
            },
        );
    }
}
