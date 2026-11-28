//! SV class lowering: `class ... endclass` registration (inherited plus
//! own fields with bit offsets, methods as `Class::method` functions,
//! constraint text for `randomize()`), and the field-access helpers used
//! by expression and assignment lowering (`$getf`/`$setf`/`$this`,
//! randomize naming, the `glbl` primitive signal). Split out of
//! `verilog_lower.rs` to keep it at its standards baseline.

use std::collections::HashMap;

use super::{real_children, ClassInfo, Lower, TypeDef, TypeKind};
use crate::ir::{Expr, FuncIr, SigId, SigKind, SignalIr};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    /// Lower a top-level `class ... endclass`: register the type, fields
    /// (inherited fields first), methods (named `Class::method`), and the raw
    /// constraint-block text used by `randomize()`.
    pub(super) fn lower_class(&mut self, c: &SyntaxNode) {
        let kids = real_children(c);
        let mut names = kids.iter().filter(|k| k.kind() == SyntaxKind::Name);
        let name = names.next().map(|k| self.text(k)).unwrap_or_default();
        let parent = names.next().map(|k| self.text(k));
        if name.is_empty() {
            return;
        }
        // Fields: inherited (parent-first) then own declarations.
        let mut fields: Vec<(String, usize, usize, bool)> = Vec::new();
        if let Some(p) = &parent {
            if let Some(pi) = self.classes.get(p) {
                fields.extend(pi.fields.iter().cloned());
            }
        }
        let mut constraint_text = String::new();
        let own = self.class_own_fields(&kids, &mut constraint_text);
        let total: usize =
            fields.iter().map(|f| f.1).sum::<usize>() + own.iter().map(|f| f.1).sum::<usize>();
        let mut lsb = total;
        for (n, w, r) in &own {
            lsb -= w;
            fields.push((n.clone(), *w, lsb, *r));
        }
        self.register_class(name.clone(), fields, total, constraint_text);
        // Lower methods with the class context set (this / bare fields).
        let methods = self.class_methods(&kids, &name);
        if let Some(ci) = self.classes.get_mut(&name) {
            ci.methods = methods;
        }
    }

    /// Collect the class's own field declarations (`rand` flag + width)
    /// and accumulate the constraint-block text.
    fn class_own_fields(
        &mut self,
        kids: &[SyntaxNode],
        constraint_text: &mut String,
    ) -> Vec<(String, usize, bool)> {
        let mut own: Vec<(String, usize, bool)> = Vec::new();
        for k in kids {
            match k.kind() {
                SyntaxKind::Decl => {
                    let dk = real_children(k);
                    let width = self.decl_width(&dk, &HashMap::new());
                    let is_rand = dk.iter().any(|d| {
                        d.kind() == SyntaxKind::Ident
                            && matches!(self.text(d).as_str(), "rand" | "randc")
                    });
                    for d in &dk {
                        if d.kind() == SyntaxKind::Name {
                            own.push((self.text(d), width, is_rand));
                        }
                    }
                }
                SyntaxKind::Constraint => {
                    let text = self.db.span_text(k.span());
                    // Strip the outer braces and the constraint name.
                    let inner = text
                        .trim_start_matches(|ch| ch != '{')
                        .trim_start_matches('{');
                    let inner = inner.trim_end_matches('}').trim();
                    if !constraint_text.is_empty() {
                        constraint_text.push_str(" ; ");
                    }
                    constraint_text.push_str(inner);
                }
                _ => {}
            }
        }
        own
    }

    /// Register the class type (64-bit handle) and its fields, width, and
    /// constraints before the methods are lowered.
    fn register_class(
        &mut self,
        name: String,
        fields: Vec<(String, usize, usize, bool)>,
        total: usize,
        constraint_text: String,
    ) {
        // Register the class (fields only) before lowering methods so bare
        // field references inside methods resolve via `cur_class_field`.
        self.types.insert(
            name.clone(),
            TypeDef {
                width: 64, // handle
                signed: false,
                real: false,
                kind: TypeKind::Class(name.clone()),
            },
        );
        self.classes.insert(
            name.clone(),
            ClassInfo {
                name: name.clone(),
                fields,
                width: total.max(1),
                methods: Vec::new(),
                constraint_text,
            },
        );
    }

    /// Lower the class's methods/tasks with the class context set
    /// (`this` / bare field references resolve via `cur_class_field`).
    fn class_methods(&mut self, kids: &[SyntaxNode], name: &str) -> Vec<FuncIr> {
        let mut methods = Vec::new();
        for k in kids {
            if k.kind() == SyntaxKind::Function || k.kind() == SyntaxKind::Task {
                let saved = self.cur_class.replace(name.to_string());
                let f = self.lower_function_ir(k, &HashMap::new());
                self.cur_class = saved;
                methods.push(f);
            }
        }
        methods
    }

    /// `glbl.GSR` / `glbl.GRESTORE` / `glbl.GTS`: the Xilinx `glbl` primitive is
    /// auto-instantiated by the tools, so a hierarchical read of it materializes
    /// a per-module 1-bit signal on first reference.
    pub(super) fn glbl_sig(&mut self, field: &str) -> SigId {
        let name = format!("glbl.{}", field);
        if let Some(id) = self.names.get(&name) {
            return *id;
        }
        let id = (self.extra_base + self.extra_signals.len()) as SigId;
        self.extra_signals.push(SignalIr {
            name: name.clone(),
            width: 1,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            signed: false,
        });
        self.names.insert(name, id);
        id
    }

    /// Allocate (once per module) the object-memory signal for class `cn`.
    pub(super) fn class_mem_sig(&mut self, cn: &str) -> Option<SigId> {
        if let Some(id) = self.class_mems.get(cn) {
            return Some(*id);
        }
        let ci = self.classes.get(cn)?.clone();
        let id = (self.extra_base + self.extra_signals.len()) as SigId;
        self.extra_signals.push(SignalIr {
            name: format!("__objmem_{}", cn),
            width: ci.width,
            kind: SigKind::Wire,
            init: None,
            mem: Some(0), // dynamic: one word per object
            signed: false,
        });
        self.sig_is_mem.insert(id);
        self.class_mems.insert(cn.to_string(), id);
        Some(id)
    }

    /// `obj.field` where obj is a class-typed signal → `$getf::C::field(obj)`.
    /// Returns None if not a class field access.
    pub(super) fn lower_field_read(
        &mut self,
        _root: &str,
        field: &str,
        root_id: SigId,
    ) -> Option<Expr> {
        let tn = self.sig_types.get(&root_id)?.clone();
        let cn = match self.types.get(&tn)?.kind {
            TypeKind::Class(ref cn) => cn.clone(),
            _ => return None,
        };
        if !self
            .classes
            .get(&cn)?
            .fields
            .iter()
            .any(|(n, _, _, _)| n == field)
        {
            return None;
        }
        Some(Expr::Call {
            name: self
                .getf_name(&cn, field)
                .unwrap_or_else(|| format!("$getf::{}::{}", cn, field)),
            args: vec![Expr::Sig(root_id)],
            shapes: Vec::new(),
        })
    }

    /// Object expression for `this` / `super` (inherited fields are merged
    /// into the same word, so both are the current object).
    pub(super) fn this_expr(&self) -> Expr {
        Expr::Call {
            name: "$this".to_string(),
            args: vec![],
            shapes: Vec::new(),
        }
    }

    /// `$getf::C::field@lsb,width` — the field layout is encoded in the call
    /// name so the simulator can extract field bits without a side table.
    pub(super) fn getf_name(&self, cn: &str, field: &str) -> Option<String> {
        let f = self
            .classes
            .get(cn)?
            .fields
            .iter()
            .find(|(n, _, _, _)| n == field)?;
        Some(format!("$getf::{}::{}@{},{}", cn, field, f.2, f.1))
    }

    /// `$setf::C::field@lsb,width` — see [`Lower::getf_name`].
    pub(super) fn setf_name(&self, cn: &str, field: &str) -> Option<String> {
        let f = self
            .classes
            .get(cn)?
            .fields
            .iter()
            .find(|(n, _, _, _)| n == field)?;
        Some(format!("$setf::{}::{}@{},{}", cn, field, f.2, f.1))
    }

    /// `$randomize::C@x=lsb,w;y=lsb,w` — rand-field layout, same order as the
    /// domains emitted into the constraint spec.
    pub(super) fn randomize_name(&self, cn: &str) -> String {
        let mut lay: Vec<String> = Vec::new();
        if let Some(ci) = self.classes.get(cn) {
            for (n, w, lsb, r) in &ci.fields {
                if *r {
                    lay.push(format!("{}={},{}", n, lsb, w));
                }
            }
        }
        format!("$randomize::{}@{}", cn, lay.join(";"))
    }

    /// Is `name` a field of the class whose method is being lowered?
    pub(super) fn cur_class_field(&self, name: &str) -> Option<String> {
        let cn = self.cur_class.as_ref()?;
        let ci = self.classes.get(cn)?;
        if ci.fields.iter().any(|(n, _, _, _)| n == name) {
            Some(cn.clone())
        } else {
            None
        }
    }

    /// If `lhs` is a class field write (`c.x` or bare `x` inside a method),
    /// return (class, field, object-expr).
    pub(super) fn class_field_lhs(&mut self, l: &SyntaxNode) -> Option<(String, String, Expr)> {
        if l.kind() == SyntaxKind::Name {
            let name = self.text(l);
            if let Some(dot) = name.find('.') {
                let root = &name[..dot];
                let field = &name[dot + 1..];
                if let Some(id) = self.sig_id(root) {
                    if let Some(tn) = self.sig_types.get(&id).cloned() {
                        if let Some(td) = self.types.get(&tn).cloned() {
                            if let TypeKind::Class(cn) = &td.kind {
                                if self.classes.get(cn).is_some_and(|ci| {
                                    ci.fields.iter().any(|(n, _, _, _)| n == field)
                                }) {
                                    return Some((cn.clone(), field.to_string(), Expr::Sig(id)));
                                }
                            }
                        }
                    }
                }
            } else if let Some(cn) = self.cur_class_field(&name) {
                return Some((cn, name, self.this_expr()));
            }
        }
        None
    }

    /// Constraint spec text for `randomize()`: rand field domains plus the
    /// class's constraint blocks.
    pub(super) fn randomize_spec(&self, cn: &str) -> String {
        let mut spec = String::new();
        if let Some(ci) = self.classes.get(cn) {
            for (n, w, _, r) in &ci.fields {
                if *r {
                    let hi = if *w >= 64 { i64::MAX } else { (1i64 << w) - 1 };
                    spec.push_str(&format!("{}: 0..{}; ", n, hi));
                }
            }
            if !ci.constraint_text.is_empty() {
                spec.push_str(&ci.constraint_text);
            }
        }
        spec
    }
}
