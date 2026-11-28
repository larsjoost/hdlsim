//! Type-declaration lowering: user types and subtypes, physical units and literals, view declarations, builtin-type registration, and resolve_type.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// `attribute name : type;` — register the attribute name as user-defined
    /// so `x'name` lowers to X without a warning. Returns the name.
    pub(super) fn register_attr_decl(&mut self, d: &SyntaxNode) -> Option<String> {
        let name = real_children(d)
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .find(|w| w != "attribute")?;
        self.attr_decls.insert(name.clone());
        Some(name)
    }

    pub(super) fn register_builtin_types(&mut self) {
        Rc::make_mut(&mut self.consts).insert(
            "true".into(),
            Value {
                bits: vec![V1],
                real: false,
            },
        );
        Rc::make_mut(&mut self.consts).insert(
            "false".into(),
            Value {
                bits: vec![V0],
                real: false,
            },
        );
        // severity_level literals (std.standard) — passed to vital_*/assert
        // helper procedures as plain values, e.g. `MsgSeverity => ERROR`.
        Rc::make_mut(&mut self.consts).insert("note".into(), Value::from_u64(32, 0));
        Rc::make_mut(&mut self.consts).insert("warning".into(), Value::from_u64(32, 1));
        Rc::make_mut(&mut self.consts).insert("error".into(), Value::from_u64(32, 2));
        Rc::make_mut(&mut self.consts).insert("failure".into(), Value::from_u64(32, 3));
        // std.textio `justified` literals (the `write(l, v, left, 13)` form).
        Rc::make_mut(&mut self.consts).insert("left".into(), Value::from_u64(32, 0));
        Rc::make_mut(&mut self.consts).insert("right".into(), Value::from_u64(32, 1));
        self.types.insert("std_logic".into(), TypeInfo::Scalar);
        // std_ulogic is the enum from ieee.std_logic_1164; hdlsim models it as
        // a 1-bit scalar like std_logic (the `use` clause is not needed).
        self.types.insert("std_ulogic".into(), TypeInfo::Scalar);
        self.types.insert("bit".into(), TypeInfo::Scalar);
        self.types.insert("boolean".into(), TypeInfo::Scalar);
        self.types.insert(
            "std_logic_vector".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types.insert(
            "std_ulogic_vector".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types
            .insert("bit_vector".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("unsigned".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("signed".into(), TypeInfo::Vector { signed: true });
        self.types
            .insert("integer".into(), TypeInfo::Integer { signed: true });
        self.types.insert("real".into(), TypeInfo::Real);
        self.types
            .insert("natural".into(), TypeInfo::Integer { signed: false });
        self.types
            .insert("positive".into(), TypeInfo::Integer { signed: false });
        self.types.insert("char".into(), TypeInfo::Char);
        self.types.insert("character".into(), TypeInfo::Char);
        self.types
            .insert("string".into(), TypeInfo::Vector { signed: false });
        self.register_char_lits();
        // std.standard file_open_kind / file_open_status (VHDL-2008).
        for (n, v) in [
            ("read_mode", 0u64),
            ("write_mode", 1),
            ("append_mode", 2),
            ("read_write_mode", 3),
            ("open_ok", 0),
            ("status_error", 1),
            ("name_error", 2),
            ("mode_error", 3),
            ("state_open", 0),
            ("state_closed", 1),
        ] {
            Rc::make_mut(&mut self.consts).insert(n.to_string(), Value::from_u64(32, v));
        }
        // VHDL-2019 range-record direction literals (`direction => descending`
        // in a `T'range'record` aggregate).
        for (n, v) in [("ascending", 0u64), ("descending", 1)] {
            Rc::make_mut(&mut self.consts).insert(n.to_string(), Value::from_u64(32, v));
        }
        self.types.insert("file_open_kind".into(), TypeInfo::Scalar);
        self.types
            .insert("file_open_status".into(), TypeInfo::Scalar);
        self.types.insert("severity_level".into(), TypeInfo::Scalar);
        // std.env (VHDL-2008): resolution_limit and the tool/version constants,
        // as packed strings (MSB-first, 8 bits per char).
        Rc::make_mut(&mut self.consts).insert("resolution_limit".into(), Value::from_u64(64, 1));
        for (n, s) in [
            ("vhdl_version", "2019"),
            ("tool_type", "simulator"),
            ("tool_vendor", "hdlsim"),
            ("tool_name", "hdlsim"),
            ("tool_edition", ""),
            ("tool_version", env!("CARGO_PKG_VERSION")),
        ] {
            let mut bits = Vec::with_capacity(s.len() * 8);
            for b in s.bytes() {
                for i in (0..8).rev() {
                    bits.push(if b >> i & 1 == 1 { V1 } else { V0 });
                }
            }
            Rc::make_mut(&mut self.consts).insert(n.to_string(), Value { bits, real: false });
        }
        // std.env assert API (VHDL-2008) — stubs so `alias x is std.env.f[...]`
        // resolves.
        for (n, ret) in [
            ("to_string", 32usize),
            ("isvhdlassertfailed", 1usize),
            ("getvhdlassertcount", 32),
            ("clearvhdlassert", 1),
            ("setvhdlassertenable", 1),
            ("getvhdlassertenable", 1),
            ("setvhdlassertformat", 1),
            ("getvhdlassertformat", 32),
            ("setvhdlreadseverity", 1),
            ("getvhdlreadseverity", 32),
        ] {
            self.register_func(FuncIr {
                name: n.to_string(),
                ret_width: ret,
                args: Vec::new(),
                defaults: Vec::new(),
                modes: Vec::new(),
                locals: Vec::new(),
                dyn_locals: Vec::new(),
                body: Stmt::Null,
                hidden: Vec::new(),
            });
        }
        // std.standard delay_length and the VHDL-2019 vector types.
        self.types
            .insert("delay_length".into(), TypeInfo::Integer { signed: false });
        self.types
            .insert("integer_vector".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("real_vector".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("time_vector".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("boolean_vector".into(), TypeInfo::Vector { signed: false });
        // ieee.std_logic_1164 (VHDL-2019) unresolved vector types.
        self.types.insert(
            "unresolved_unsigned".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types.insert(
            "unresolved_signed".into(),
            TypeInfo::Vector { signed: true },
        );
        // std.textio: `line` is a string buffer (simulated as a wide vector,
        // see resolve_type); `text` is a file object (its name is bound to the
        // file path by lower_file_decl, not a signal).
        self.types
            .insert("line".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("text".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("time".into(), TypeInfo::Integer { signed: false });
    }

    /// Resolve a type mark + optional constraint to (width, signed, mem_depth).
    /// Type mark of an interface element, given the children after the `:`:
    /// the first Name, skipping a VHDL-2019 `view [(] ViewName [)] of` prefix.
    pub(super) fn iface_type_mark<'k>(&self, after: &'k [SyntaxNode]) -> Option<&'k SyntaxNode> {
        let is_view = after
            .first()
            .is_some_and(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "view");
        if is_view {
            let of = after
                .iter()
                .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "of")?;
            after[of + 1..]
                .iter()
                .find(|k| k.kind() == SyntaxKind::Name)
        } else {
            after.iter().find(|k| k.kind() == SyntaxKind::Name)
        }
    }

    /// VHDL-2019 view declaration: register the view name as an alias of its
    /// subtype's type, so `port (bus : view V of T)` elements and `V'converse`
    /// aliases resolve to `T`.
    pub(super) fn lower_view_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        let Some(name_node) = kids.iter().find(|k| k.kind() == SyntaxKind::Name) else {
            return;
        };
        let name = self.kw(name_node);
        let of_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "of");
        let tm = of_pos.and_then(|p| kids[p + 1..].iter().find(|k| k.kind() == SyntaxKind::Name));
        let ti = match tm {
            Some(tm) => {
                let tname = self.kw(tm);
                self.types
                    .get(&tname)
                    .cloned()
                    .or_else(|| {
                        let last = tname.split('.').next_back().unwrap_or(tname.as_str());
                        self.types.get(last).cloned()
                    })
                    .unwrap_or(TypeInfo::Scalar)
            }
            None => TypeInfo::Scalar,
        };
        self.types.insert(name, ti);
    }

    pub(super) fn resolve_type(
        &mut self,
        mark: &SyntaxNode,
        constraint: Option<&SyntaxNode>,
        span: Span,
    ) -> (usize, bool, Option<usize>, bool) {
        let name = self.kw(mark);
        let base = self.types.get(&name).cloned().or_else(|| {
            // Selected type mark `lib.pkg.type` (e.g. ieee.numeric_std.unsigned):
            // look the item up in the package, falling back to the bare item
            // name (builtin/std types like `unsigned` are pre-registered and
            // thus not re-exported by the package).
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() >= 2 {
                let last = parts[parts.len() - 1];
                if let Some(p) = self.pkgs.get(parts[parts.len() - 2]) {
                    if let Some((_, v)) = p.types.iter().find(|(k, _)| k == last) {
                        return Some(v.clone());
                    }
                }
                self.types.get(last).cloned()
            } else {
                None
            }
        });
        let Some(base) = base else {
            // `X'subtype` type mark where X is a port/signal (not a type):
            // adopt the object's width as an anonymous vector.
            if let Some(id) = self.sig_id(&name) {
                let w = self.sig_width.get(&id).copied().unwrap_or(1).max(1);
                return (w, false, None, false);
            }
            // Selected type mark `obj.field[.field...]` (`AxiBus.WriteAddress'subtype`,
            // `LocalWD.Data'subtype`): resolve the base object (signal/port or
            // record alias) and adopt the field's width. A record-typed field
            // (e.g. `AxiBus.WriteAddress`) yields the record's total width;
            // `as_record` on the same dotted name gives the record type for
            // member selects on the declared object.
            if let Some((b, rest)) = name.split_once('.') {
                let path: Vec<&str> = rest.split('.').collect();
                let tn = self
                    .sig_id(b)
                    .and_then(|i| self.sig_rec.get(&i).cloned())
                    .or_else(|| self.alias_rec.get(b).cloned());
                if let Some(tn) = tn {
                    if let Some((_, w)) = self.record_field_offset(&tn, &path) {
                        return (w.max(1), false, None, false);
                    }
                }
            }
            if self.suppress_err == 0 {
                self.diags.push(Diagnostic {
                    code: "E0401",
                    severity: Severity::Error,
                    span: Some(span),
                    message: format!("unknown type '{}'", name),
                    help: Some(
                        "known type marks: bit, boolean, character, integer, natural, positive, \
                         real, time, string, bit_vector, std_logic, std_ulogic, std_logic_vector, \
                         std_ulogic_vector, unsigned, signed \
                         (std.standard / ieee.std_logic_1164 / ieee.numeric_std)"
                            .to_string(),
                    ),
                });
            }
            return (1, false, None, false);
        };
        match base {
            TypeInfo::Vector { signed } => {
                // An unconstrained `line` (std.textio) is a string buffer of
                // up to TEXTIO_LINE_CAP bytes.
                if name == "line" && constraint.is_none() {
                    return (TEXTIO_LINE_CAP * 8, signed, None, false);
                }
                let w = constraint
                    .map(|c| self.constraint_width(c))
                    .unwrap_or_else(|| self.type_widths.get(&name).copied().unwrap_or(1))
                    .max(1);
                // Strings pack 8 bits per character (like string literals).
                let w = if name == "string" { w * 8 } else { w };
                (w, signed, None, false)
            }
            TypeInfo::Integer { signed } => (32, signed, None, false),
            TypeInfo::Real => (64, false, None, true),
            TypeInfo::Scalar => (1, false, None, false),
            TypeInfo::Char => (8, false, None, false),
            TypeInfo::Enum(ref lits) => {
                let w = bits_for(lits.len());
                (w, false, None, false)
            }
            TypeInfo::Array {
                elem, depth, dims, ..
            } => {
                // A constrained array type mark (`array (field_id) of
                // field_t`) covers every element of its own dims: the flat
                // width is elem × ∏ sizes, not the bare element width —
                // otherwise objects of the type collapse to one element and
                // named-choice/OTHERS aggregates cannot re-size them.
                // Unconstrained marks (empty dims) keep the element width;
                // the object's own constraint sizes those.
                let w = if dims.is_empty() {
                    elem
                } else {
                    elem.max(1) * dims.iter().map(|(_, n)| (*n).max(1)).product::<usize>()
                };
                // An unconstrained array type mark with an object index
                // constraint (`arr : rem_n_array_t(0 to 7)` of 1-bit
                // vectors) sizes the memory depth from that constraint —
                // the declared depth is the placeholder 1, and leaving it
                // collapses every element onto row 0 (whole-array assigns
                // then compare equal every sweep and the object freezes).
                let depth = if dims.is_empty() {
                    constraint
                        .map(|c| {
                            // Best-effort depth estimate: `resolve_type`
                            // re-runs in contexts where the constraint's
                            // bound names are not in scope (protected-body
                            // shared variables re-resolved outside their
                            // body context), and the estimate must not
                            // report spurious E0401s there — unresolvable
                            // bounds fall back to the declared depth, like
                            // the width estimator.
                            self.suppress_err += 1;
                            let n = self.constraint_width(c);
                            self.suppress_err -= 1;
                            n
                        })
                        .filter(|n| *n > 1)
                        .unwrap_or(depth)
                } else {
                    depth
                };
                (w, false, Some(depth), false)
            }
            TypeInfo::Record { fields } => (
                fields.iter().map(|f| f.1).sum::<usize>().max(1),
                false,
                None,
                false,
            ),
            TypeInfo::Handle => (64, false, None, false),
        }
    }

    pub(super) fn lower_type_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        // kids[0] is the `type` keyword; the name is the next ident.
        let Some(name_node) = kids.iter().filter(|k| k.kind() == SyntaxKind::Ident).nth(1) else {
            return;
        };
        let name = self.kw(name_node);
        // Incomplete type `type t;` — completed later in a package body;
        // register a placeholder so uses of the name resolve.
        if !kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "is")
        {
            self.types.insert(name, TypeInfo::Scalar);
            return;
        }
        // Access type: `type p is access t;` — opaque handle placeholder
        // (`.all` deref is handled at the use site); remember the designated
        // type so `p.field` on an access-to-record resolves as implicit `.all`.
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "access")
        {
            let access_pos = kids
                .iter()
                .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "access");
            if let Some(ap) = access_pos {
                if let Some(tm) = kids[ap + 1..].iter().find(|k| k.kind() == SyntaxKind::Name) {
                    let inner = self.kw(tm);
                    self.access_inner.insert(name.clone(), inner.clone());
                    // If the designated type is already known, adopt its
                    // TypeInfo so `p.field` on an access-to-record lowers as an
                    // implicit `.all` field select at the use site.
                    if let Some(ti) = self.types.get(&inner).cloned() {
                        self.types.insert(name.clone(), ti);
                        return;
                    }
                }
            }
            self.types.insert(name, TypeInfo::Scalar);
            return;
        }
        // File type: `type f is file of elem;` — opaque handle placeholder.
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "file")
        {
            self.types.insert(name, TypeInfo::Scalar);
            return;
        }
        // Protected type (VHDL-2008): `type t is protected ... end protected;`
        // declares the methods; `type t is protected body ... end protected
        // body;` holds the state variables and method bodies. Objects are
        // 64-bit handles (same object model as SV classes); state lives in the
        // object word, methods lower as `t::m` functions.
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "protected")
        {
            let is_body = kids
                .iter()
                .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "body");
            self.types.insert(name.clone(), TypeInfo::Handle);
            let mut pt = self.prot_types.remove(&name).unwrap_or_default();
            // First pass: inner type decls + state variables, so the type is
            // registered (with its fields) before any initializer/method body
            // is lowered (a state-variable initializer may reference an
            // earlier field of the same type).
            let mut init_decls: Vec<(String, SyntaxNode)> = Vec::new();
            for k in &kids {
                match k.kind() {
                    SyntaxKind::TypeDecl => self.lower_type_decl(k),
                    // Body-local subtype (`subtype t_data_buffer is
                    // std_logic_vector(N-1 downto 0);`) — must be registered
                    // before the state variables that use it.
                    SyntaxKind::SubtypeDecl => self.lower_subtype_decl(k),
                    SyntaxKind::VariableDecl if is_body => {
                        let vk = real_children(k);
                        let colon = vk
                            .iter()
                            .position(|c| c.kind() == SyntaxKind::Punct && self.text(c) == ":");
                        if let Some(c) = colon {
                            let names: Vec<String> = vk[..c]
                                .iter()
                                .filter(|c| c.kind() == SyntaxKind::Name)
                                .map(|c| self.kw(c))
                                .collect();
                            let tmark = vk[c + 1..].iter().find(|c| c.kind() == SyntaxKind::Name);
                            let constraint =
                                vk[c + 1..].iter().find(|c| c.kind() == SyntaxKind::Range);
                            let init = vk[c + 1..]
                                .iter()
                                .find(|c| is_expr_node(c.kind()) && c.kind() != SyntaxKind::Name);
                            let (w, signed, _, _) = match tmark {
                                Some(tm) => self.resolve_type(tm, constraint, k.span()),
                                None => (32, false, None, false),
                            };
                            for n in names {
                                pt.fields.push((n.clone(), w.max(1), 0, signed));
                                // Field initializer (for the `t::new` ctor).
                                if let Some(ie) = init {
                                    init_decls.push((n, ie.clone()));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Assign lsb offsets: first declared field is the MSB of the word.
            let total: usize = pt.fields.iter().map(|f| f.1).sum::<usize>().max(1);
            let mut run = 0usize;
            for f in pt.fields.iter_mut() {
                f.2 = total - run - f.1;
                run += f.1;
            }
            // Register before lowering initializers/methods: they reference
            // the fields (bare names resolve to `$getf::t::field@lsb,w($this)`).
            self.prot_types.insert(name.clone(), pt.clone());
            {
                let saved = self.prot_body_ctx.replace(name.clone());
                for (n, ie) in init_decls {
                    let ex = self.lower_expr(&ie, &self.consts.clone());
                    pt.inits.push((n, ex));
                }
                self.prot_body_ctx = saved;
            }
            // Second pass: methods (stubs in the header, real bodies in the
            // protected body, lowered with the protected context so bare
            // state-var references resolve to `$getf::t::field@lsb,w($this)`).
            for k in &kids {
                if k.kind() != SyntaxKind::Function {
                    continue;
                }
                // Method name: first Name before `return`/`begin`.
                let mname = {
                    let mut seen = false;
                    let mut mn = String::new();
                    for c in real_children(k) {
                        match c.kind() {
                            SyntaxKind::Ident
                                if matches!(self.kw(&c).as_str(), "return" | "begin") =>
                            {
                                seen = true
                            }
                            SyntaxKind::Name if !seen && mn.is_empty() => mn = self.text(&c),
                            _ => {}
                        }
                    }
                    mn.to_ascii_lowercase()
                };
                if mname.is_empty() {
                    continue;
                }
                if is_body {
                    let saved = self.prot_body_ctx.replace(name.clone());
                    let fi = self.lower_function_node(k);
                    self.prot_body_ctx = saved;
                    if let Some(mut fi) = fi {
                        fi.name = format!("{}::{}", name, fi.name);
                        self.register_func(fi.clone());
                        self.pending_funcs.push(fi);
                    }
                } else {
                    self.protected_method_stub(k);
                }
                if !pt.methods.contains(&mname) {
                    pt.methods.push(mname);
                }
            }
            // Constructor: `t::new` runs after `$new::t` allocates the object;
            // it writes each state variable's initializer into the word.
            let mut ctor_stmts: Vec<Stmt> = Vec::new();
            for (fn_, w, lsb, _) in &pt.fields {
                if let Some((_, ex)) = pt.inits.iter().find(|(n, _)| n == fn_) {
                    ctor_stmts.push(Stmt::SysCall {
                        name: format!("$setf::{}::{}@{},{}", name, fn_, lsb, w),
                        args: vec![
                            Expr::Call {
                                name: "$this".into(),
                                args: vec![],
                                shapes: Vec::new(),
                            },
                            ex.clone(),
                        ],
                        shapes: Vec::new(),
                    });
                }
            }
            let ctor = FuncIr {
                name: format!("{}::new", name),
                ret_width: 1,
                args: Vec::new(),
                defaults: Vec::new(),
                modes: Vec::new(),
                locals: Vec::new(),
                dyn_locals: Vec::new(),
                body: Stmt::Block(ctor_stmts),
                hidden: Vec::new(),
            };
            self.register_func(ctor.clone());
            self.pending_funcs.push(ctor);
            self.prot_types.insert(name, pt);
            return;
        }
        // Record: `type t is record f : typ [constraint]; ... end record;`
        // (checked before enum: field constraints contain `(` too).
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "record")
        {
            // Fields: `name [, name...] : type [constraint];` — comma-separated
            // field names share one type mark.
            let mut fields: Vec<(String, usize, bool)> = Vec::new();
            let mut i = 0;
            while i < kids.len() {
                if kids[i].kind() == SyntaxKind::Name {
                    let mut j = i;
                    let mut names: Vec<String> = Vec::new();
                    while j < kids.len() && kids[j].kind() == SyntaxKind::Name {
                        names.push(self.text(&kids[j]).to_ascii_lowercase());
                        j += 1;
                        if kids
                            .get(j)
                            .is_some_and(|p| p.kind() == SyntaxKind::Punct && self.text(p) == ",")
                        {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if kids
                        .get(j)
                        .is_some_and(|p| p.kind() == SyntaxKind::Punct && self.text(p) == ":")
                    {
                        if let Some(tm) = kids.get(j + 1).filter(|k| k.kind() == SyntaxKind::Name) {
                            let constraint =
                                kids.get(j + 2).filter(|k| k.kind() == SyntaxKind::Range);
                            let (w, signed, _, _) = self.resolve_type(tm, constraint, d.span());
                            // Unconstrained array type mark with a decl-site
                            // constraint (`leaves : leaf_vec_t(0 to 3)`):
                            // resolve_type returns the element width for
                            // arrays, but a record field's flat slice must
                            // cover every element the constraint adds
                            // (constrained subtypes carry their sizes in
                            // dims and need no extra factor).
                            let ftn = self.kw(tm);
                            let w =
                                match self.types.get(&ftn) {
                                    Some(TypeInfo::Array { dims, .. })
                                        if dims.is_empty() && constraint.is_some() =>
                                    {
                                        w.max(1)
                                            * self
                                                .constraint_width(constraint.expect(
                                                    "match guard checked constraint.is_some()",
                                                ))
                                                .max(1)
                                    }
                                    _ => w,
                                };
                            let fty = self.kw(tm);
                            for n in names {
                                self.rec_field_ty
                                    .insert((name.clone(), n.clone()), fty.clone());
                                // Field of a multi-bit-element array type with
                                // a decl constraint: record the effective dims
                                // so chain indexes flatten and field slices
                                // remap with the right low bound (an
                                // unconstrained array type has none, and a
                                // string subtype's range is lost when the
                                // field resolves to the bare type).
                                let multi_elem =
                                    matches!(
                                        self.types.get(&ftn),
                                        Some(TypeInfo::Array { dims, .. }) if dims.is_empty()
                                    ) || self.array_elem_width(&ftn).is_some_and(|ew| ew > 1);
                                if let (true, Some(c)) = (multi_elem, constraint) {
                                    let lo = self.range_low(c, &self.consts.clone()).unwrap_or(0);
                                    let cnt = self.constraint_width(c).max(1);
                                    self.rec_field_dims
                                        .insert((name.clone(), n.clone()), vec![(lo, cnt)]);
                                }
                                fields.push((n, w.max(1), signed));
                            }
                            i = j + 2;
                            continue;
                        }
                    }
                }
                i += 1;
            }
            if !fields.is_empty() {
                self.types.insert(name, TypeInfo::Record { fields });
            }
            return;
        }
        // Enum: `type t is (a, b, c);`
        let enum_kids: Vec<&SyntaxNode> = kids
            .iter()
            .skip_while(|k| !(k.kind() == SyntaxKind::Punct && self.text(k) == "("))
            .filter(|k| k.kind() == SyntaxKind::Ident || k.kind() == SyntaxKind::CharLiteral)
            .collect();
        let enum_lits: Vec<String> = enum_kids
            .iter()
            .map(|k| self.text(k).trim_matches('\'').to_ascii_lowercase())
            .collect();
        if !enum_lits.is_empty() {
            for (i, k) in enum_kids.iter().enumerate() {
                // Char-literal labels ('U', '0', ...) lex as CharLiteral, never
                // as bare names — registering them as consts would shadow
                // same-named signals (e.g. a signal `u` vs std_ulogic's 'U').
                if k.kind() == SyntaxKind::Ident {
                    Rc::make_mut(&mut self.consts)
                        .insert(enum_lits[i].clone(), Value::from_u64(32, i as u64));
                }
            }
            self.types.insert(name, TypeInfo::Enum(enum_lits));
            return;
        }
        // Array: `type t is array (range) of elem;`
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "array")
        {
            let range = kids.iter().find(|k| k.kind() == SyntaxKind::Range);
            let elem_pos = kids.iter().rposition(|k| k.kind() == SyntaxKind::Name);
            let elem = elem_pos.map(|i| kids[i].clone());
            // Element constraint, if any: `... of std_logic_vector(31 downto 0)`.
            let elem_constraint = elem_pos.and_then(|i| {
                kids.iter()
                    .skip(i + 1)
                    .find(|k| k.kind() == SyntaxKind::Range)
            });
            // An unconstrained index (`array (natural range <>) of ...`) has
            // no dims: the object's constraint or the folded value sizes it.
            // `constraint_dims` would report a (0, 1) unknown-sentinel dim,
            // which materialized constants would take as a real 1-element
            // constraint.
            let unconstrained = range.map(|r| self.text(r).contains("<>")).unwrap_or(true);
            let dims = if unconstrained {
                Vec::new()
            } else {
                range.map(|r| self.constraint_dims(r)).unwrap_or_default()
            };
            let depth = if dims.is_empty() {
                range.map(|r| self.constraint_width(r)).unwrap_or(1).max(1)
            } else {
                dims.iter().map(|d| d.1).product::<usize>().max(1)
            };
            let elem_name = elem.as_ref().map(|e| self.text(e).to_ascii_lowercase());
            let (ew, _, _, _) = match elem {
                Some(e) => self.resolve_type(&e, elem_constraint, d.span()),
                None => (8, false, None, false),
            };
            self.types.insert(
                name,
                TypeInfo::Array {
                    elem: ew,
                    depth,
                    dims,
                    elem_name,
                },
            );
            return;
        }
        // Range type: `type t is range 0 to 10;` → integer
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "range")
        {
            // The ieee fixed/float types (`type ufixed is range <>;`) map to
            // real; plain integer ranges stay integer.
            if matches!(name.as_str(), "ufixed" | "sfixed" | "float") {
                self.types.insert(name, TypeInfo::Real);
            } else {
                self.types.insert(name, TypeInfo::Integer { signed: false });
            }
            // Physical type: `type d is range 0 to 1e16 units ... end units;`
            // -- record the unit values so literals (`1.5 cm`) and bare unit
            // names (`mm`) lower to base-unit integers.
            if kids
                .iter()
                .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "units")
            {
                self.lower_phys_units(&kids);
            }
        }
    }

    /// Physical type body: `units base; name = value unit; ... end units;`
    /// The parser keeps the entries as flat children of the TypeDecl.
    /// Records each unit's value in the base unit in `phys_units`.
    pub(super) fn lower_phys_units(&mut self, kids: &[SyntaxNode]) {
        let Some(upos) = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "units")
        else {
            return;
        };
        let mut i = upos + 1;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident && self.kw(k) == "end" {
                break;
            }
            if k.kind() != SyntaxKind::Name {
                i += 1;
                continue;
            }
            let uname = self.text(k).to_ascii_lowercase();
            let is_derived = kids
                .get(i + 1)
                .is_some_and(|p| p.kind() == SyntaxKind::Punct && self.text(p) == "=");
            if !is_derived {
                // Base (primary) unit: value 1.
                self.phys_units.insert(uname, 1);
                i += 1;
                continue;
            }
            // Derived unit: `name = <value> [unit] ;` -- the value may be a
            // merged physical literal (`10 mm`), a number plus a separate
            // unit name (`10 angstrom` for unit names outside the parser's
            // built-in list), or a general expression.
            let mut j = i + 2;
            let mut val: Option<u64> = None;
            if let Some(e) = kids.get(j) {
                match e.kind() {
                    SyntaxKind::Literal => {
                        let t = self.text(e);
                        val = match t.split_once(char::is_whitespace) {
                            Some((num, unit)) => self.phys_lit_value(num, unit),
                            None => self
                                .parse_int_literal(t.trim())
                                .and_then(|v| v.to_u64())
                                .or_else(|| t.trim().parse::<f64>().ok().map(|f| f as u64)),
                        };
                        j += 1;
                    }
                    _ if is_expr_node(e.kind()) => {
                        if let Expr::Lit(v) = self.lower_expr(e, &self.consts.clone()) {
                            val = v.to_u64();
                        }
                        j += 1;
                    }
                    _ => {}
                }
            }
            // Separate unit name after a plain number: `nm = 10 angstrom;`
            if let Some(u) = kids.get(j).filter(|u| u.kind() == SyntaxKind::Name) {
                let un = self.text(u).to_ascii_lowercase();
                let base = self
                    .phys_units
                    .get(&un)
                    .copied()
                    .unwrap_or_else(|| time_unit_fs(&un));
                val = val.map(|v| v.saturating_mul(base));
            }
            self.phys_units.insert(uname, val.unwrap_or(1));
            // Skip to the terminating `;` of this unit entry.
            while i < kids.len()
                && !(kids[i].kind() == SyntaxKind::Punct && self.text(&kids[i]) == ";")
            {
                i += 1;
            }
            i += 1;
        }
    }

    /// Value of a physical literal `num unit` in base units (the unit is
    /// either a user-declared physical unit or a built-in time/length/freq
    /// unit).
    pub(super) fn phys_lit_value(&mut self, num: &str, unit: &str) -> Option<u64> {
        let ul = unit.trim().to_ascii_lowercase();
        let base = self
            .phys_units
            .get(&ul)
            .copied()
            .unwrap_or_else(|| time_unit_fs(&ul));
        let n = self
            .parse_int_literal(num.trim())
            .and_then(|v| v.to_u64())
            .or_else(|| num.trim().parse::<f64>().ok().map(|f| f.max(0.0) as u64));
        n.map(|n| n.saturating_mul(base))
    }

    pub(super) fn lower_subtype_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        // kids[0] is the `subtype` keyword; the name is the next ident.
        let name = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .nth(1)
            .map(|k| self.kw(k));
        let Some(name) = name else { return };
        // `subtype s is base [(constraint)] [range ...]`
        let is_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "is");
        let Some(is_pos) = is_pos else { return };
        let base = kids[is_pos + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .cloned();
        let constraint = kids[is_pos + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range)
            .cloned();
        if let Some(base) = base {
            let span = d.span();
            let base_name = self.kw(&base);
            let (w, signed, mem, is_real) = self.resolve_type(&base, constraint.as_ref(), span);
            let _ = mem;
            // Keep record/array base types as-is; a REAL subtype stays REAL
            // (LRM 1076 §5.3: a subtype has its base's type — the range
            // constraint does not change the representation, so
            // `subtype POSITIVE_REAL is REAL range 0.0 to REAL'HIGH`
            // must still be a 64-bit real, not a width-from-range vector).
            match self.types.get(&base_name).cloned() {
                Some(mut ti @ (TypeInfo::Record { .. } | TypeInfo::Array { .. })) => {
                    // An array subtype's constraint bounds its objects:
                    // carry it in the dims so signal declarations,
                    // aggregates, and `'length` see the subtype's range,
                    // not the unconstrained base's. A box `<>` constraint
                    // adds nothing — keep the base dims.
                    if let TypeInfo::Array { depth, dims, .. } = &mut ti {
                        if let Some(c) = &constraint {
                            if !self.text(c).contains("<>") {
                                let cd = self.constraint_dims(c);
                                if !cd.is_empty() {
                                    *depth = cd.iter().map(|(_, s)| (*s).max(1)).product();
                                    *dims = cd;
                                }
                            }
                        }
                    }
                    self.types.insert(name, ti);
                }
                _ if is_real => {
                    self.types.insert(name, TypeInfo::Real);
                }
                _ => {
                    if constraint.is_some() {
                        self.type_widths.insert(name.clone(), w.max(1));
                        // Keep the constrained range for vector subtypes:
                        // aggregates onto such signals and their `'range`
                        // loops need the low bound, not only the width.
                        if matches!(self.types.get(&base_name), Some(TypeInfo::Vector { .. })) {
                            if let Some(c) = &constraint {
                                if !self.text(c).contains("<>") {
                                    let cd = self.constraint_dims(c);
                                    if !cd.is_empty() {
                                        self.type_dims.insert(name.clone(), cd);
                                    }
                                }
                            }
                        }
                    }
                    // Element width of a Vector-based subtype: `string`
                    // subtypes are character arrays (8), others 1 —
                    // `array_elem_width` consults this for value-shape
                    // folds (`dims_from_init`). Recorded before `name`
                    // moves into `types`.
                    if let Some(e) = self.array_elem_width(&base_name) {
                        self.type_elem.insert(name.clone(), e);
                    }
                    self.types.insert(name, TypeInfo::Vector { signed });
                }
            }
        }
    }
}
