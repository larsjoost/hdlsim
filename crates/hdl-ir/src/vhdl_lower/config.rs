//! Configuration-declaration lowering and binding application: ConfigIr construction, for-spec paths, and generic/port binding onto instances.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Lower a configuration declaration into a [`ConfigIr`].
    pub(super) fn lower_config_decl(&mut self, c: &SyntaxNode) -> ConfigIr {
        let kids = real_children(c);
        let name = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .find(|w| !matches!(w.as_str(), "configuration" | "of" | "is" | "end"))
            .unwrap_or_default();
        let entity = kids
            .iter()
            .skip_while(|k| self.kw(k) != "of")
            .nth(1)
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .unwrap_or_default();
        let entity = entity.rsplit('.').next().unwrap_or("").to_string();
        let mut cfg = ConfigIr {
            name,
            entity,
            arch: None,
            blocks: Vec::new(),
        };
        for k in &kids {
            if k.kind() == SyntaxKind::ForSpec {
                self.lower_config_for(k, &mut cfg, Vec::new());
            }
        }
        cfg
    }

    /// One `for <block spec> ... end for;` inside a configuration declaration.
    /// A bare top-level `for arch ... end for;` (no binding) sets the default
    /// architecture; nested blocks under it are recorded relative to the top.
    pub(super) fn lower_config_for(
        &mut self,
        fs: &SyntaxNode,
        cfg: &mut ConfigIr,
        mut path: Vec<String>,
    ) {
        let kids = real_children(fs);
        let mut all = false;
        let mut others = false;
        for k in &kids {
            if k.kind() == SyntaxKind::Ident {
                let w = self.kw(k);
                if w == "all" {
                    all = true;
                } else if w == "others" {
                    others = true;
                } else if w == "use" || w == "end" {
                    break;
                }
            } else if k.kind() == SyntaxKind::Name {
                path.push(self.text(k).to_ascii_lowercase());
            } else if k.kind() == SyntaxKind::Punct {
                continue; // `:` between label and component name
            } else {
                break;
            }
        }
        // If this for-block has a binding use directly, record it.
        let has_binding = kids.iter().any(|k| k.kind() == SyntaxKind::UseClause);
        if !has_binding && path.len() == 1 && !all && !others {
            // `for arch ... end for;` — default architecture binding.
            cfg.arch = Some(path[0].clone());
        }
        let child_base = if !has_binding && path.len() == 1 && !all && !others {
            Vec::new() // relative to the configuration's top
        } else {
            path.clone()
        };
        for k in &kids {
            match k.kind() {
                SyntaxKind::ForSpec => self.lower_config_for(k, cfg, child_base.clone()),
                SyntaxKind::UseClause => {
                    let binding = self.lower_binding_kids(&real_children(k));
                    cfg.blocks.push(BlockCfg {
                        path: child_base.clone(),
                        all,
                        others,
                        binding,
                    });
                }
                _ => {}
            }
        }
    }

    /// A binding indication's children (a UseClause node in a configuration, or
    /// the inline tail of a for-specification): `use [entity|configuration]
    /// name [(arch)] [generic map (...)] [port map (...)]`.
    pub(super) fn lower_binding_kids(&mut self, kids: &[SyntaxNode]) -> Binding {
        let is_config = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "configuration");
        // `name` or `name(arch)` — the latter parses as Call(Name, "(", arch, ")").
        let (name, arch) = match kids.iter().find(|k| k.kind() == SyntaxKind::Call) {
            Some(call) => {
                let ck = real_children(call);
                let un = ck
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))
                    .unwrap_or_default();
                let arch = ck
                    .iter()
                    .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "(")
                    .and_then(|p| ck.get(p + 1))
                    .filter(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.kw(k));
                (un, arch)
            }
            None => (
                kids.iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))
                    .unwrap_or_default(),
                None,
            ),
        };
        let unit = name.rsplit('.').next().unwrap_or("").to_string();
        if is_config {
            return Binding::Configuration {
                name: unit.to_ascii_lowercase(),
            };
        }
        let mut generic_map = Vec::new();
        let mut port_map = Vec::new();
        let mut in_generic = false;
        for k in kids {
            if k.kind() == SyntaxKind::Ident {
                let w = self.kw(k);
                if w == "generic" {
                    in_generic = true;
                } else if w == "port" {
                    in_generic = false;
                }
            } else if k.kind() == SyntaxKind::PortConn {
                let ck = real_children(k);
                let name_pos = self.assoc_formal(&ck);
                // Indexed formal `i_data(0 to 1) => x`: keep the base name.
                let assoc = name_pos.map(|i| {
                    let t = self.text(&ck[i]);
                    let t = t.rsplit('.').next().unwrap_or(t.as_str());
                    let end = t.find('(').unwrap_or(t.len());
                    t[..end].to_string()
                });
                let expr = ck
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(i, c)| Some(*i) != name_pos && is_expr_node(c.kind()))
                    .map(|(_, c)| c);
                let Some(e) = expr else { continue };
                self.suppress_err += 1;
                let mut ex = self.lower_expr(e, &self.consts.clone());
                self.suppress_err -= 1;
                if in_generic {
                    ex = self.fold_generic_actual(ex);
                }
                match (assoc, in_generic) {
                    (Some(n), true) => generic_map.push((Some(n), ex)),
                    (Some(n), false) => port_map.push(Conn::Named(n, ex)),
                    // Positional generic actuals bind to the target's
                    // generics in declaration order; the name is resolved
                    // at elaboration, where the entity's parameter list is
                    // visible (resolve_positional_params).
                    (None, true) => generic_map.push((None, ex)),
                    (None, false) => port_map.push(Conn::Positional(ex)),
                }
            }
        }
        Binding::Entity {
            name: unit,
            arch,
            generic_map,
            port_map,
        }
    }

    /// Generic actuals are static expressions (VHDL evaluates them at
    /// elaboration): fold what const-evals to a literal, so generic-map
    /// variants and child parameter binding carry values instead of
    /// parent-scope references the elaborator cannot chase. Non-constant
    /// actuals pass through unchanged.
    pub(super) fn fold_generic_actual(&self, ex: Expr) -> Expr {
        match self.ce_env(&ex, &self.consts.clone()) {
            Some(v) => Expr::Lit(v),
            None => ex,
        }
    }

    /// Apply a binding indication to an instance (configuration
    /// specification `for label : comp use ...;` or a block configuration).
    pub(super) fn apply_binding(&mut self, inst: &mut InstIr, binding: &Binding, span: Span) {
        match binding {
            Binding::Entity {
                name,
                arch,
                generic_map,
                port_map,
            } => {
                inst.module = match arch {
                    Some(a) => format!("{name}/{a}"),
                    None => name.clone(),
                };
                inst.param_overrides.extend(generic_map.iter().cloned());
                if !port_map.is_empty() {
                    inst.conns = port_map.clone();
                }
            }
            Binding::Configuration { name } => {
                if let Some(cfg) = self.cfgs.get(&name.to_ascii_lowercase()).cloned() {
                    inst.module = match &cfg.arch {
                        Some(a) => format!("{}/{}", cfg.entity, a),
                        None => cfg.entity.clone(),
                    };
                    inst.config = Some(name.clone());
                } else {
                    self.err(span, format!("unknown configuration '{name}' in binding"));
                }
            }
        }
    }
}
