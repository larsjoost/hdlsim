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
    /// Module instantiation (`foo bar (...)`).
    Inst,
    /// One port connection in an instantiation (`.name(expr)` or positional).
    PortConn,
    /// One `#(...)` parameter override: `.name(expr)` or positional expr.
    ParamAssign,

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
    /// Aggregate `(a, b, others => c)`.
    Aggregate,
    /// Attribute `x'attr`.
    Attr,
    /// Qualified expression `type'(expr)`.
    Qualified,
    /// `others` in aggregates/case.
    Others,
}

impl SyntaxKind {
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
                | SyntaxKind::Block
                | SyntaxKind::If
                | SyntaxKind::Case
                | SyntaxKind::CaseItem
                | SyntaxKind::Wait
                | SyntaxKind::For
                | SyntaxKind::SensList
                | SyntaxKind::Inst
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
