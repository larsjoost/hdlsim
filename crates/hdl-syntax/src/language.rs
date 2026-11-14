//! The language registry — the extension seam for adding languages or dialects.
//!
//! See `docs/extensibility.md`: a new language/dialect is a new module
//! implementing [`LanguageFrontend`] plus one entry in [`languages`]. Dialect
//! differences (e.g. SystemVerilog vs Verilog-2005) are expressed as a
//! `DialectFlags` config passed to the same parser, not as forked parsers.

use crate::diagnostic::Diagnostic;
use crate::source::{FileId, SourceFile};
use crate::token::Token;
use std::path::Path;

/// A hardware description language understood by hdlsim.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Language {
    Verilog,
    Vhdl,
}

impl Language {
    /// Guess the language from a file extension: `.v`/`.sv` → Verilog,
    /// `.vhd`/`.vhdl` → VHDL.
    pub fn from_path(path: &Path) -> Option<Language> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "v" | "sv" => Some(Language::Verilog),
            "vhd" | "vhdl" => Some(Language::Vhdl),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::Verilog => "verilog",
            Language::Vhdl => "vhdl",
        }
    }
}

/// Extension seam: implement for a new language or dialect, register in
/// [`languages`].
///
/// Implementations must never panic on arbitrary input; all problems are
/// returned as diagnostics.
pub trait LanguageFrontend {
    fn language(&self) -> Language;

    /// Lex a whole file into tokens (including whitespace/comment tokens and a
    /// trailing `Eof`), plus any lexing diagnostics.
    fn lex(&self, file: FileId, src: &SourceFile) -> (Vec<Token>, Vec<Diagnostic>);
}

/// Registry of known languages. Add one entry per language/dialect.
pub fn languages() -> &'static [Language] {
    &[Language::Verilog, Language::Vhdl]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_extension() {
        assert_eq!(
            Language::from_path(Path::new("a.v")),
            Some(Language::Verilog)
        );
        assert_eq!(
            Language::from_path(Path::new("a.sv")),
            Some(Language::Verilog)
        );
        assert_eq!(
            Language::from_path(Path::new("a.vhd")),
            Some(Language::Vhdl)
        );
        assert_eq!(
            Language::from_path(Path::new("a.VHDL")),
            Some(Language::Vhdl)
        );
        assert_eq!(Language::from_path(Path::new("a.txt")), None);
    }
}
