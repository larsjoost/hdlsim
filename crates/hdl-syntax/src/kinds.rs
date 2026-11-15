//! The syntax-kind vocabulary: one closed enum covering every token and node kind
//! of every language. Token kinds mirror [`crate::token::TokenKind`] 1:1 via
//! [`SyntaxKind::from_token`]; node kinds are added per language as its parser
//! grows (verilog::*, vhdl::*).

use crate::token::TokenKind;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SyntaxKind {
    // --- tokens (mirror TokenKind) ---
    Ident,
    IntLiteral,
    RealLiteral,
    StrLiteral,
    /// VHDL character literal (`'0'`).
    CharLiteral,
    /// `$name` system task/function.
    SysTask,
    /// `` `name `` compiler directive / macro reference.
    Directive,
    /// One punctuation character.
    Punct,
    Whitespace,
    Comment,
    /// A character or sequence the lexer could not classify.
    Error,
    Eof,

    // --- generic nodes ---
    /// Root of a file's tree. Invariant: a tree has exactly one Root.
    Root,
    /// A token the parser could not place into the grammar (recovery node).
    ErrorToken,

    // --- verilog declarations and module structure ---
    Module,
    PortList,
    Port,
    /// `wire`/`reg`/`logic`/`integer`/`real`/`genvar` declaration.
    Decl,
    /// `[msb:lsb]` or `[width]` range.
    Range,
    /// `parameter`/`localparam` declaration.
    ParamDecl,
    /// Continuous `assign`.
    Assign,
    /// `always` block, optionally with a `@(...)` sensitivity list.
    Always,
    /// The `@(...)` sensitivity list of an `always` block.
    SensList,
    Initial,
    Function,
    Generate,
    GenFor,
    GenIf,
    GenBlock,
    /// `specify ... endspecify` timing block (parsed, not simulated).
    Specify,
    /// Module instantiation (`foo bar (...)`) or gate primitive (`and (o,a,b)`).
    Inst,
    /// `assert property (@(clk) a |-> b);` concurrent assertion (SVA-lite).
    AssertProperty,
    /// `assume property (...);` — parsed, not simulated (SVA-lite stub).
    AssumeProperty,
    /// `cover property (...);` — parsed, not simulated (SVA-lite stub).
    CoverProperty,
    /// Property expression inside assert/assume/cover: `ante |-> cons`,
    /// `ante |=> cons`, or a plain boolean expr.
    Property,
    /// One port connection in an instantiation (`.name(expr)` or positional).
    PortConn,
    /// One `#(...)` parameter override: `.name(expr)` or positional expr.
    ParamAssign,
    /// `task [automatic] name [(ports)]; ... endtask`.
    Task,
    /// `event name, ...;`
    EventDecl,
    /// `defparam path.param = expr, ...;`
    Defparam,
    /// `final` block (SystemVerilog).
    Final,
    /// `typedef type name;` (incl. enum/struct/union bodies).
    Typedef,
    /// `enum { ... }` type body.
    EnumType,
    /// `struct/union [packed] { ... }` type body.
    StructType,
    /// Cast: `signed'(e)`, `unsigned'(e)`, or size cast `N'(e)`.
    Cast,
    /// SV immediate assertion: `assert/assume/cover/expect (...) ...`.
    SvaStmt,
    /// `generate case ... endcase endgenerate`.
    GenCase,

    // --- verilog statements ---
    /// Begin/end or single statement.
    Block,
    If,
    Case,
    CaseItem,
    /// Blocking (`=`) or non-blocking (`<=`) procedural assignment.
    StmtAssign,
    /// `#delay` prefix.
    Delay,
    /// `@(...)` / `@sig` / `@*` event wait in procedural code.
    Wait,
    /// Procedural `for` loop (vs. generate-for `GenFor`).
    For,
    /// `while (c) s`, `repeat (n) s`, `forever s`, `do s while (c)`.
    While,
    Repeat,
    Forever,
    DoWhile,
    /// `break;`
    BreakStmt,
    /// `continue;`
    ContinueStmt,
    /// `disable name;`
    Disable,
    /// `fork ... join` / `join_any` / `join_none`.
    ForkJoin,
    /// Event trigger: `-> name;` / `->> name;`.
    Trigger,
    /// System task call (`$display(...)`).
    SysCall,

    // --- verilog expressions ---
    BinExpr,
    UnaryExpr,
    /// `cond ? a : b`.
    CondExpr,
    /// `f(args)` — function call or system function.
    Call,
    /// `{a, b, c}` concatenation.
    Concat,
    /// `{n{expr}}` replication.
    Replicate,
    /// `expr[msb:lsb]` part-select.
    PartSelect,
    /// `expr[idx]` indexing.
    Index,
    /// `(expr)` grouping.
    ParenExpr,
    /// A (possibly dotted/hierarchical) name.
    Name,
    /// Any literal.
    Literal,

    // --- vhdl structure ---
    /// `library x; use y.z.all;` clauses.
    LibraryClause,
    UseClause,
    /// `entity name is ... end [entity] [name];`
    Entity,
    /// `generic (...)` / `port (...)` clauses of an entity.
    GenericClause,
    PortClause,
    /// One `name : mode type` entry in a port/generic clause.
    PortDecl,
    GenericDecl,
    /// `architecture name of entity is ... begin ... end [name];`
    Architecture,
    /// `package [body] name is ... end [name];`
    Package,
    /// Declarative items of an architecture/entity/package body.
    SignalDecl,
    ConstantDecl,
    VariableDecl,
    ComponentDecl,
    TypeDecl,
    SubtypeDecl,
    /// `process [(sens)] ... begin ... end process;` (concurrent).
    Process,
    /// Concurrent signal assignment `a <= b;`.
    ConcAssign,
    /// Sequential signal assignment `a <= b;` inside a process.
    SeqAssign,
    /// Sequential variable assignment `a := b;`.
    VarAssign,
    /// `assert ... report ... severity ...;` / `report ...`.
    AssertStmt,
    /// `null;`
    NullStmt,
    /// `loop`/`while`/`for` — `for` reuses For; this is `loop`/`while`.
    LoopStmt,
    /// `exit;` / `next;`.
    ExitStmt,
    /// Procedure call statement `p(args);`.
    CallStmt,
    /// `return [expr];` in a subprogram body.
    ReturnStmt,
    /// `configuration name of entity is ... end;` declaration.
    ConfigDecl,
    /// VHDL-2008 context clause: `context lib.name;`.
    ContextClause,
    /// VHDL-2008 package instantiation: `package p is new g generic map (...);`.
    PackageInst,
    /// `alias name [: type] is target;`.
    AliasDecl,
    /// `attribute a : type;` declaration.
    AttrDecl,
    /// `attribute a of target : label is expr;` specification.
    AttrSpec,
    /// File object declaration: `file f : text open read_mode is "x";`.
    FileDecl,
    /// Component configuration specification: `for lbl : comp use ...;`.
    ForSpec,
    /// Concurrent selected signal assignment: `with sel select t <= ...;`.
    SelectedAssign,
    /// External name (VHDL-2008): `<< signal dut.s : std_logic >>`.
    ExtName,
    /// Allocator: `new type_mark [ (expr) | '(expr) ]`.
    Allocator,
    /// Aggregate `(a, b, others => c)`.
    Aggregate,
    /// Attribute `x'attr`.
    Attr,
    /// Qualified expression `type'(expr)`.
    Qualified,
    /// `others` in aggregates/case.
    Others,
    /// Marker for the `?` in a VHDL-2008 matching case (`case? ... is`).
    Matching,
}

/// All variants in declaration order — index == `kind as u16`.
/// Fingerprint of the kind table (FNV-1a over the variant names). Cache
/// entries that embed kind indices (e.g. the `.hdlsim-cache/` parse cache)
/// compare against this, so any change to the table invalidates them.
pub fn fingerprint() -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for k in ALL {
        for b in format!("{:?}", k).as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

pub const ALL: &[SyntaxKind] = &[
    SyntaxKind::Ident,
    SyntaxKind::IntLiteral,
    SyntaxKind::RealLiteral,
    SyntaxKind::StrLiteral,
    SyntaxKind::CharLiteral,
    SyntaxKind::SysTask,
    SyntaxKind::Directive,
    SyntaxKind::Punct,
    SyntaxKind::Whitespace,
    SyntaxKind::Comment,
    SyntaxKind::Error,
    SyntaxKind::Eof,
    SyntaxKind::Root,
    SyntaxKind::ErrorToken,
    SyntaxKind::Module,
    SyntaxKind::PortList,
    SyntaxKind::Port,
    SyntaxKind::Decl,
    SyntaxKind::Range,
    SyntaxKind::ParamDecl,
    SyntaxKind::Assign,
    SyntaxKind::Always,
    SyntaxKind::SensList,
    SyntaxKind::Initial,
    SyntaxKind::Function,
    SyntaxKind::Generate,
    SyntaxKind::GenFor,
    SyntaxKind::GenIf,
    SyntaxKind::GenBlock,
    SyntaxKind::Specify,
    SyntaxKind::Inst,
    SyntaxKind::AssertProperty,
    SyntaxKind::AssumeProperty,
    SyntaxKind::CoverProperty,
    SyntaxKind::Property,
    SyntaxKind::PortConn,
    SyntaxKind::ParamAssign,
    SyntaxKind::Task,
    SyntaxKind::EventDecl,
    SyntaxKind::Defparam,
    SyntaxKind::Final,
    SyntaxKind::Typedef,
    SyntaxKind::EnumType,
    SyntaxKind::StructType,
    SyntaxKind::Cast,
    SyntaxKind::SvaStmt,
    SyntaxKind::GenCase,
    SyntaxKind::Block,
    SyntaxKind::If,
    SyntaxKind::Case,
    SyntaxKind::CaseItem,
    SyntaxKind::StmtAssign,
    SyntaxKind::Delay,
    SyntaxKind::Wait,
    SyntaxKind::For,
    SyntaxKind::While,
    SyntaxKind::Repeat,
    SyntaxKind::Forever,
    SyntaxKind::DoWhile,
    SyntaxKind::BreakStmt,
    SyntaxKind::ContinueStmt,
    SyntaxKind::Disable,
    SyntaxKind::ForkJoin,
    SyntaxKind::Trigger,
    SyntaxKind::SysCall,
    SyntaxKind::BinExpr,
    SyntaxKind::UnaryExpr,
    SyntaxKind::CondExpr,
    SyntaxKind::Call,
    SyntaxKind::Concat,
    SyntaxKind::Replicate,
    SyntaxKind::PartSelect,
    SyntaxKind::Index,
    SyntaxKind::ParenExpr,
    SyntaxKind::Name,
    SyntaxKind::Literal,
    SyntaxKind::LibraryClause,
    SyntaxKind::UseClause,
    SyntaxKind::Entity,
    SyntaxKind::GenericClause,
    SyntaxKind::PortClause,
    SyntaxKind::PortDecl,
    SyntaxKind::GenericDecl,
    SyntaxKind::Architecture,
    SyntaxKind::Package,
    SyntaxKind::SignalDecl,
    SyntaxKind::ConstantDecl,
    SyntaxKind::VariableDecl,
    SyntaxKind::ComponentDecl,
    SyntaxKind::TypeDecl,
    SyntaxKind::SubtypeDecl,
    SyntaxKind::Process,
    SyntaxKind::ConcAssign,
    SyntaxKind::SeqAssign,
    SyntaxKind::VarAssign,
    SyntaxKind::AssertStmt,
    SyntaxKind::NullStmt,
    SyntaxKind::LoopStmt,
    SyntaxKind::ExitStmt,
    SyntaxKind::CallStmt,
    SyntaxKind::ReturnStmt,
    SyntaxKind::ConfigDecl,
    SyntaxKind::ContextClause,
    SyntaxKind::PackageInst,
    SyntaxKind::AliasDecl,
    SyntaxKind::AttrDecl,
    SyntaxKind::AttrSpec,
    SyntaxKind::FileDecl,
    SyntaxKind::ForSpec,
    SyntaxKind::SelectedAssign,
    SyntaxKind::ExtName,
    SyntaxKind::Allocator,
    SyntaxKind::Aggregate,
    SyntaxKind::Attr,
    SyntaxKind::Qualified,
    SyntaxKind::Others,
    SyntaxKind::Matching,
];

impl SyntaxKind {
    /// Stable numeric index of this kind (declaration order == discriminant).
    pub fn to_index(self) -> u16 {
        self as u16
    }

    /// Inverse of [`SyntaxKind::to_index`].
    pub fn from_index(v: u16) -> Option<SyntaxKind> {
        ALL.get(v as usize).copied()
    }

    pub fn from_token(kind: TokenKind) -> SyntaxKind {
        match kind {
            TokenKind::Ident => SyntaxKind::Ident,
            TokenKind::IntLiteral => SyntaxKind::IntLiteral,
            TokenKind::RealLiteral => SyntaxKind::RealLiteral,
            TokenKind::StrLiteral => SyntaxKind::StrLiteral,
            TokenKind::CharLiteral => SyntaxKind::CharLiteral,
            TokenKind::SysTask => SyntaxKind::SysTask,
            TokenKind::Directive => SyntaxKind::Directive,
            TokenKind::Punct => SyntaxKind::Punct,
            TokenKind::Whitespace => SyntaxKind::Whitespace,
            TokenKind::Comment => SyntaxKind::Comment,
            TokenKind::Error => SyntaxKind::Error,
            TokenKind::Eof => SyntaxKind::Eof,
        }
    }

    pub fn is_token(self) -> bool {
        matches!(
            self,
            SyntaxKind::Ident
                | SyntaxKind::IntLiteral
                | SyntaxKind::RealLiteral
                | SyntaxKind::StrLiteral
                | SyntaxKind::CharLiteral
                | SyntaxKind::SysTask
                | SyntaxKind::Directive
                | SyntaxKind::Punct
                | SyntaxKind::Whitespace
                | SyntaxKind::Comment
                | SyntaxKind::Error
                | SyntaxKind::Eof
        )
    }

    /// Whether this kind is a verilog node (used by AST views and golden dumps).
    pub fn is_verilog_node(self) -> bool {
        matches!(
            self,
            SyntaxKind::Module
                | SyntaxKind::PortList
                | SyntaxKind::Port
                | SyntaxKind::Decl
                | SyntaxKind::Range
                | SyntaxKind::ParamDecl
                | SyntaxKind::Assign
                | SyntaxKind::Always
                | SyntaxKind::SensList
                | SyntaxKind::Initial
                | SyntaxKind::Function
                | SyntaxKind::Generate
                | SyntaxKind::GenFor
                | SyntaxKind::GenIf
                | SyntaxKind::GenBlock
                | SyntaxKind::Inst
                | SyntaxKind::PortConn
                | SyntaxKind::ParamAssign
                | SyntaxKind::Task
                | SyntaxKind::EventDecl
                | SyntaxKind::Defparam
                | SyntaxKind::Final
                | SyntaxKind::Typedef
                | SyntaxKind::EnumType
                | SyntaxKind::StructType
                | SyntaxKind::Cast
                | SyntaxKind::SvaStmt
                | SyntaxKind::GenCase
                | SyntaxKind::While
                | SyntaxKind::Repeat
                | SyntaxKind::Forever
                | SyntaxKind::DoWhile
                | SyntaxKind::BreakStmt
                | SyntaxKind::ContinueStmt
                | SyntaxKind::Disable
                | SyntaxKind::ForkJoin
                | SyntaxKind::Trigger
                | SyntaxKind::ReturnStmt
                | SyntaxKind::Block
                | SyntaxKind::If
                | SyntaxKind::Case
                | SyntaxKind::CaseItem
                | SyntaxKind::StmtAssign
                | SyntaxKind::Delay
                | SyntaxKind::Wait
                | SyntaxKind::For
                | SyntaxKind::SysCall
                | SyntaxKind::BinExpr
                | SyntaxKind::UnaryExpr
                | SyntaxKind::CondExpr
                | SyntaxKind::Call
                | SyntaxKind::Concat
                | SyntaxKind::Replicate
                | SyntaxKind::PartSelect
                | SyntaxKind::Index
                | SyntaxKind::ParenExpr
                | SyntaxKind::Name
                | SyntaxKind::Literal
        )
    }
    /// Whether this kind is a VHDL node (used by AST views and golden dumps).
    pub fn is_vhdl_node(self) -> bool {
        matches!(
            self,
            SyntaxKind::LibraryClause
                | SyntaxKind::UseClause
                | SyntaxKind::Entity
                | SyntaxKind::GenericClause
                | SyntaxKind::PortClause
                | SyntaxKind::PortDecl
                | SyntaxKind::GenericDecl
                | SyntaxKind::Architecture
                | SyntaxKind::Package
                | SyntaxKind::SignalDecl
                | SyntaxKind::ConstantDecl
                | SyntaxKind::VariableDecl
                | SyntaxKind::ComponentDecl
                | SyntaxKind::TypeDecl
                | SyntaxKind::SubtypeDecl
                | SyntaxKind::Process
                | SyntaxKind::ConcAssign
                | SyntaxKind::SeqAssign
                | SyntaxKind::VarAssign
                | SyntaxKind::AssertStmt
                | SyntaxKind::NullStmt
                | SyntaxKind::LoopStmt
                | SyntaxKind::ExitStmt
                | SyntaxKind::CallStmt
                | SyntaxKind::ReturnStmt
                | SyntaxKind::Aggregate
                | SyntaxKind::Attr
                | SyntaxKind::Qualified
                | SyntaxKind::Others
                | SyntaxKind::ConfigDecl
                | SyntaxKind::ContextClause
                | SyntaxKind::PackageInst
                | SyntaxKind::AliasDecl
                | SyntaxKind::AttrDecl
                | SyntaxKind::AttrSpec
                | SyntaxKind::FileDecl
                | SyntaxKind::ForSpec
                | SyntaxKind::SelectedAssign
                | SyntaxKind::GenCase
                | SyntaxKind::ExtName
                | SyntaxKind::Allocator
                | SyntaxKind::CondExpr
                | SyntaxKind::Block
                | SyntaxKind::If
                | SyntaxKind::Case
                | SyntaxKind::CaseItem
                | SyntaxKind::Wait
                | SyntaxKind::For
                | SyntaxKind::SensList
                | SyntaxKind::Inst
                | SyntaxKind::AssertProperty
                | SyntaxKind::AssumeProperty
                | SyntaxKind::CoverProperty
                | SyntaxKind::Property
                | SyntaxKind::PortConn
                | SyntaxKind::BinExpr
                | SyntaxKind::UnaryExpr
                | SyntaxKind::Call
                | SyntaxKind::Index
                | SyntaxKind::PartSelect
                | SyntaxKind::ParenExpr
                | SyntaxKind::Name
                | SyntaxKind::Literal
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_index_roundtrips_for_all_variants() {
        assert_eq!(ALL.len(), 117);
        for (i, k) in ALL.iter().enumerate() {
            assert_eq!(k.to_index() as usize, i, "{:?} out of order", k);
            assert_eq!(SyntaxKind::from_index(k.to_index()), Some(*k));
        }
        assert_eq!(SyntaxKind::from_index(65535), None);
    }
}
