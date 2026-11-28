//! Use-clause / context resolution helpers: which libraries and design
//! units a file's `use`/`context` clauses reference (the library manager
//! compiles those units in dependency order). Split out of `vhdl_lower.rs`
//! to keep it at its standards baseline; re-exported unchanged.

use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::SourceDb;

use super::{package_decl_name, real_children};

/// Library names referenced by a library-wide use clause (`use lib.all;`,
/// `context lib.all;`) at the top of a design file — e.g. `use math.all`
/// yields `math`. The library manager compiles every unit of that library.
pub fn context_libraries(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    // Context declarations (`context c is use ...; end context;`) name their
    // clause units too — descend one level so a context file pulls in what its
    // clauses reference.
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(child) = stack.pop() {
        if child.kind() == SyntaxKind::ContextDecl {
            stack.extend(real_children(&child));
            continue;
        }
        match child.kind() {
            SyntaxKind::ContextClause | SyntaxKind::UseClause => {
                for n in real_children(&child) {
                    if n.kind() == SyntaxKind::Name {
                        let text = db.span_text(n.span()).trim();
                        let parts: Vec<&str> = text.split('.').collect();
                        if parts.len() == 2 && parts[1].eq_ignore_ascii_case("all") {
                            let lib = parts[0].to_ascii_lowercase();
                            if !lib.is_empty() && !out.contains(&lib) {
                                out.push(lib);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Unit names referenced by the context clauses (`use lib.pkg...;`,
/// `context lib.pkg...;`) at the top of a design file — e.g.
/// `use ieee.std_logic_1164.all` yields `std_logic_1164`. The library manager
/// resolves these before lowering the file so the packages are visible.
pub fn context_units(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    // Context declarations (`context c is use ...; end context;`) name their
    // clause units too — descend so a context file pulls in what its clauses
    // reference.
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(child) = stack.pop() {
        if child.kind() == SyntaxKind::ContextDecl {
            stack.extend(real_children(&child));
            continue;
        }
        match child.kind() {
            SyntaxKind::ContextClause | SyntaxKind::UseClause => {
                for n in real_children(&child) {
                    if n.kind() == SyntaxKind::Name {
                        let text = db.span_text(n.span()).trim();
                        let parts: Vec<&str> = text.split('.').collect();
                        // `lib.unit.all` / `lib.unit.item` / `unit.all` / `unit.item`
                        // Two parts are ambiguous (`lib.unit` vs `unit.item`):
                        // unless the second part is `all`, treat it as the unit
                        // (`context vunit_lib.com_context;` references the unit
                        // `com_context` in library `vunit_lib`).
                        let unit = match parts.len() {
                            n if n >= 3 => parts[n - 2],
                            2 if parts[1].eq_ignore_ascii_case("all") => parts[0],
                            2 => parts[1],
                            _ => continue,
                        };
                        if !unit.is_empty() && !out.iter().any(|u| u == unit) {
                            out.push(unit.to_ascii_lowercase());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.extend(package_inst_target_units(db, root));
    out
}

/// Lowercased names of the design units declared in this file (packages,
/// package instantiations, context declarations, entities, architectures,
/// configurations) — so resolution can skip units the file declares itself
/// (they are lowered by pass 0 of this file, and resolving them externally
/// could pull in a same-named unit from another file).
pub fn declared_unit_names(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(n) = stack.pop() {
        match n.kind() {
            SyntaxKind::Package => {
                let name = package_decl_name(&n, db).to_ascii_lowercase();
                if !name.is_empty() && !out.contains(&name) {
                    out.push(name);
                }
            }
            SyntaxKind::PackageInst | SyntaxKind::ContextDecl => {
                if let Some(id) = real_children(&n)
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| db.span_text(k.span()).trim().to_ascii_lowercase())
                    .find(|w| !matches!(w.as_str(), "package" | "body" | "context" | "is" | "new"))
                {
                    if !out.contains(&id) {
                        out.push(id);
                    }
                }
            }
            _ => stack.extend(real_children(&n)),
        }
    }
    out
}

/// Generic-package targets of all `package p is new g [generic map (...)]`
/// instantiations anywhere in the file (possibly nested in declarative
/// parts) — the first Name child of each PackageInst node (`lib.g` → `g`).
/// The generic map is not descended into: its actuals may name local items.
pub fn package_inst_target_units(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(n) = stack.pop() {
        if n.kind() == SyntaxKind::PackageInst {
            if let Some(t) = real_children(&n)
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Name)
            {
                let unit = db
                    .span_text(t.span())
                    .trim()
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !unit.is_empty() && !out.iter().any(|u| u == &unit) {
                    out.push(unit);
                }
            }
        } else {
            stack.extend(real_children(&n));
        }
    }
    out
}

/// `(unit, library)` pairs referenced by the context clauses of a design file —
/// like [`context_units`], but carrying the library qualifier of the `use`
/// clause so the library manager can resolve `use lib.unit` inside `lib`
/// (LRM 11.2) before falling back to the global search. `None` means the
/// library is unknown (package instantiations) and only the global search
/// applies. Library-wide `use lib.all;` clauses are skipped: they carry no
/// unit name and are handled via `context_libraries`.
pub fn context_unit_libs(db: &SourceDb, root: &SyntaxNode) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    // Context declarations (`context c is use ...; end context;`) name their
    // clause units too — descend so a context file pulls in what its clauses
    // reference.
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(child) = stack.pop() {
        if child.kind() == SyntaxKind::ContextDecl {
            stack.extend(real_children(&child));
            continue;
        }
        match child.kind() {
            SyntaxKind::ContextClause | SyntaxKind::UseClause => {
                for n in real_children(&child) {
                    if n.kind() == SyntaxKind::Name {
                        let text = db.span_text(n.span()).trim();
                        let parts: Vec<&str> = text.split('.').collect();
                        // Same part split as `context_units`: `lib.unit.all` /
                        // `lib.unit.item` name unit `unit` in library `lib`;
                        // bare 2-part names are ambiguous but conventionally
                        // `lib.unit` (see `context_units`).
                        let (unit, lib) = match parts.len() {
                            n if n >= 3 => (parts[n - 2], Some(parts[0])),
                            2 if parts[1].eq_ignore_ascii_case("all") => continue,
                            2 => (parts[1], Some(parts[0])),
                            _ => continue,
                        };
                        if unit.is_empty() {
                            continue;
                        }
                        let lib = lib.map(|l| l.to_ascii_lowercase());
                        let unit = unit.to_ascii_lowercase();
                        if !out.iter().any(|(u, l)| *u == unit && *l == lib) {
                            out.push((unit, lib));
                        }
                    }
                }
            }
            // `package p is new g generic map (...)` depends on `g` and on any
            // package actuals in the map (`generic map (P => work.q)`).
            SyntaxKind::PackageInst => {
                let mut stack: Vec<SyntaxNode> = real_children(&child);
                while let Some(n) = stack.pop() {
                    if n.kind() == SyntaxKind::Name {
                        let unit = db
                            .span_text(n.span())
                            .trim()
                            .rsplit('.')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if !unit.is_empty() && !out.iter().any(|(u, _)| *u == unit) {
                            out.push((unit, None));
                        }
                    }
                    stack.extend(real_children(&n));
                }
            }
            _ => {}
        }
    }
    out
}

/// Unit names referenced by a single `use lib.unit...;` clause node — used to
/// record a package's own uses so `use work.p.all` is transitive.
pub(super) fn use_clause_units(u: &SyntaxNode, db: &SourceDb) -> Vec<String> {
    let mut out = Vec::new();
    for n in real_children(u) {
        if n.kind() == SyntaxKind::Name {
            let text = db.span_text(n.span()).trim();
            let parts: Vec<&str> = text.split('.').collect();
            let unit = match parts.len() {
                n if n >= 3 => parts[n - 2],
                2 => parts[0],
                _ => continue,
            };
            let unit = unit.to_ascii_lowercase();
            if !unit.is_empty() && !out.contains(&unit) {
                out.push(unit);
            }
        }
    }
    out
}
