//! Lossless concrete syntax tree (CST).
//!
//! Hand-rolled rowan-style design: immutable green nodes behind `Rc`, with
//! lightweight cursor nodes ([`SyntaxNode`]) carrying absolute spans and a parent
//! chain. Whitespace and comments are tokens in the tree, so the exact source is
//! always reconstructible — this is what makes a built-in formatter and
//! refactoring tools possible.
//!
//! # Invariants
//! - Green nodes are immutable; a [`SyntaxNode`] cursor is just (green, span,
//!   parent), so cloning a cursor clones only the parent chain.
//! - A node's span covers exactly its children's spans, contiguously, in order.
//! - `root.text(db)` equals the whole source file.
//! - The parser builds trees exclusively through [`TreeBuilder`], which enforces
//!   the invariants mechanically.

use crate::kinds::SyntaxKind;
use crate::source::{FileId, SourceDb, Span};
use std::rc::Rc;

#[derive(Clone, Debug)]
enum GreenChild {
    Node(Rc<GreenNode>),
    Token { kind: SyntaxKind, len: u32 },
}

#[derive(Clone, Debug)]
struct GreenNode {
    kind: SyntaxKind,
    /// Total length in bytes of this node's span (sum of children lengths).
    len: u32,
    children: Vec<GreenChild>,
}

/// A cursor into the immutable green tree.
#[derive(Clone, Debug)]
pub struct SyntaxNode {
    green: Rc<GreenNode>,
    span: Span,
    parent: Option<Box<SyntaxNode>>,
}

impl SyntaxNode {
    fn from_green(green: Rc<GreenNode>, span: Span, parent: Option<Box<SyntaxNode>>) -> SyntaxNode {
        SyntaxNode {
            green,
            span,
            parent,
        }
    }

    pub fn kind(&self) -> SyntaxKind {
        self.green.kind
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn parent(&self) -> Option<&SyntaxNode> {
        self.parent.as_deref()
    }

    /// The source text of this node (lossless: leaves are exact source slices).
    pub fn text<'a>(&self, db: &'a SourceDb) -> &'a str {
        db.span_text(self.span)
    }

    /// Direct children (nodes and tokens), in source order.
    pub fn children(&self) -> Children {
        Children {
            parent: self.clone(),
            iter: self.green.children.clone().into_iter(),
            offset: self.span.start,
        }
    }

    /// All descendants in pre-order (self first).
    pub fn descendants(&self) -> Descendants {
        Descendants {
            stack: vec![self.clone()],
        }
    }

    /// First direct child with the given kind, if any.
    pub fn child(&self, kind: SyntaxKind) -> Option<SyntaxNode> {
        self.children().find(|c| c.kind() == kind)
    }
}

pub struct Children {
    parent: SyntaxNode,
    iter: std::vec::IntoIter<GreenChild>,
    offset: u32,
}

impl Iterator for Children {
    type Item = SyntaxNode;

    fn next(&mut self) -> Option<SyntaxNode> {
        let child = self.iter.next()?;
        // Tokens have no green node of their own; wrap a synthetic leaf so that
        // every cursor is uniform (kind + span + parent).
        let (len, green) = match child {
            GreenChild::Node(g) => (g.len, g),
            GreenChild::Token { kind, len } => (
                len,
                Rc::new(GreenNode {
                    kind,
                    len,
                    children: Vec::new(),
                }),
            ),
        };
        let span = Span::new(self.parent.span.file, self.offset, self.offset + len);
        self.offset += len;
        Some(SyntaxNode::from_green(
            green,
            span,
            Some(Box::new(self.parent.clone())),
        ))
    }
}

pub struct Descendants {
    stack: Vec<SyntaxNode>,
}

impl Iterator for Descendants {
    type Item = SyntaxNode;

    fn next(&mut self) -> Option<SyntaxNode> {
        let node = self.stack.pop()?;
        // Push children in reverse so they pop in source order.
        for child in node.children().collect::<Vec<_>>().into_iter().rev() {
            self.stack.push(child);
        }
        Some(node)
    }
}

/// Incrementally builds a green tree from parser events. Enforces the invariants:
/// tokens only inside an open node, spans contiguous, one root at `finish`.
pub struct TreeBuilder {
    file: FileId,
    offset: u32,
    stack: Vec<OpenNode>,
    root: Option<Rc<GreenNode>>,
}

struct OpenNode {
    kind: SyntaxKind,
    start: u32,
    children: Vec<GreenChild>,
}

impl TreeBuilder {
    pub fn new(file: FileId) -> TreeBuilder {
        TreeBuilder {
            file,
            offset: 0,
            stack: Vec::new(),
            root: None,
        }
    }

    pub fn start_node(&mut self, kind: SyntaxKind) {
        self.stack.push(OpenNode {
            kind,
            start: self.offset,
            children: Vec::new(),
        });
    }

    /// Append a token of `kind` covering the next `len` bytes of source.
    pub fn token(&mut self, kind: SyntaxKind, len: u32) {
        if self.stack.is_empty() {
            // Defensive: tokens before the root still land in an implicit Root.
            self.start_node(SyntaxKind::Root);
        }
        self.offset += len;
        self.stack
            .last_mut()
            .expect("stack non-empty: just ensured")
            .children
            .push(GreenChild::Token { kind, len });
    }

    pub fn finish_node(&mut self) {
        let open = self
            .stack
            .pop()
            .expect("finish_node without start_node: TreeBuilder invariant violated");
        let len = self.offset - open.start;
        let node = Rc::new(GreenNode {
            kind: open.kind,
            len,
            children: open.children,
        });
        match self.stack.last_mut() {
            Some(top) => top.children.push(GreenChild::Node(node)),
            None => self.root = Some(node),
        }
    }

    /// Close any still-open nodes (error recovery) and return the root cursor.
    pub fn finish(mut self) -> SyntaxNode {
        while !self.stack.is_empty() {
            self.finish_node();
        }
        let root = self
            .root
            .expect("TreeBuilder::finish called with no nodes: TreeBuilder invariant violated");
        SyntaxNode::from_green(root, Span::new(self.file, 0, self.offset), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceDb;

    fn db_with(src: &str) -> SourceDb {
        let mut db = SourceDb::new();
        db.add_file("t.v", src).unwrap();
        db
    }

    /// Build: Root { node { Ident "module", ws, Ident "m", Punct ";" } }
    fn sample_tree(file: FileId) -> SyntaxNode {
        let mut b = TreeBuilder::new(file);
        b.start_node(SyntaxKind::Root);
        b.start_node(SyntaxKind::Ident); // reuse any kind as a node for the test
        b.token(SyntaxKind::Ident, 6); // "module"
        b.token(SyntaxKind::Whitespace, 1);
        b.token(SyntaxKind::Ident, 1); // "m"
        b.token(SyntaxKind::Punct, 1); // ";"
        b.finish_node();
        b.finish_node();
        b.finish()
    }

    #[test]
    fn spans_cover_children_contiguously() {
        let root = sample_tree(FileId(0));
        assert_eq!(root.kind(), SyntaxKind::Root);
        assert_eq!(root.span(), Span::new(FileId(0), 0, 9));
        let kids: Vec<_> = root.children().collect();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].span(), Span::new(FileId(0), 0, 9));
        let leaves: Vec<_> = kids[0].children().collect();
        assert_eq!(leaves.len(), 4);
        assert_eq!(leaves[0].span(), Span::new(FileId(0), 0, 6));
        assert_eq!(leaves[3].span(), Span::new(FileId(0), 8, 9));
    }

    #[test]
    fn text_reconstructs_source() {
        let src = "module m; // hi\nendmodule";
        let db = db_with(src);
        let mut b = TreeBuilder::new(FileId(0));
        b.start_node(SyntaxKind::Root);
        b.token(SyntaxKind::Ident, 6);
        b.token(SyntaxKind::Whitespace, 1);
        b.token(SyntaxKind::Ident, 1);
        b.token(SyntaxKind::Punct, 1);
        b.token(SyntaxKind::Whitespace, 1);
        b.token(SyntaxKind::Comment, 5);
        b.token(SyntaxKind::Whitespace, 1);
        b.token(SyntaxKind::Ident, 9);
        b.finish_node();
        let root = b.finish();
        assert_eq!(root.text(&db), src);
    }

    #[test]
    fn descendants_are_pre_order() {
        let db = db_with("ab cd");
        let mut b = TreeBuilder::new(FileId(0));
        b.start_node(SyntaxKind::Root);
        b.start_node(SyntaxKind::Ident);
        b.token(SyntaxKind::Ident, 2);
        b.finish_node();
        b.token(SyntaxKind::Whitespace, 1);
        b.start_node(SyntaxKind::Ident);
        b.token(SyntaxKind::Ident, 2);
        b.finish_node();
        b.finish_node();
        let root = b.finish();
        let kinds: Vec<SyntaxKind> = root.descendants().map(|n| n.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                SyntaxKind::Root,
                SyntaxKind::Ident,
                SyntaxKind::Ident,
                SyntaxKind::Whitespace,
                SyntaxKind::Ident,
                SyntaxKind::Ident,
            ]
        );
        let _ = db;
    }

    #[test]
    fn parent_chain_walks_up() {
        let db = db_with("ab");
        let mut b = TreeBuilder::new(FileId(0));
        b.start_node(SyntaxKind::Root);
        b.start_node(SyntaxKind::Ident);
        b.token(SyntaxKind::Ident, 2);
        b.finish_node();
        b.finish_node();
        let root = b.finish();
        let inner = root.children().next().unwrap();
        let leaf = inner.children().next().unwrap();
        assert_eq!(leaf.parent().unwrap().kind(), SyntaxKind::Ident);
        assert_eq!(
            leaf.parent().unwrap().parent().unwrap().kind(),
            SyntaxKind::Root
        );
        assert!(root.parent().is_none());
        let _ = db;
    }
}
