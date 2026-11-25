//! Multi-file source database and byte spans.
//!
//! All positions in trees, tokens, and diagnostics are byte offsets into a
//! [`SourceFile`] registered in a [`SourceDb`]. Offsets are `u32` — a single
//! source file larger than 4 GiB is rejected at registration, not handled.

/// A byte range within one source file: `text[start..end]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Span {
        Span { file, start, end }
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Index of a file in a [`SourceDb`]. Valid for the lifetime of the db.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FileId(pub u32);

/// One named source file. Invariant: `text.len() <= u32::MAX` (enforced in
/// [`SourceDb::add_file`]).
#[derive(Clone, Debug)]
pub struct SourceFile {
    name: String,
    text: String,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceFile {
        SourceFile {
            name: name.into(),
            text: text.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 1-based (line, column) of a byte offset, for diagnostics.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let offset = (offset as usize).min(self.text.len());
        let mut line = 1u32;
        let mut col = 1u32;
        for (i, ch) in self.text.char_indices() {
            if i >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

/// All source files of a compilation session. `Clone` so CLI observers can
/// hold their own handle to render event spans as `file:line`.
#[derive(Clone, Default, Debug)]
pub struct SourceDb {
    files: Vec<SourceFile>,
}

impl SourceDb {
    pub fn new() -> SourceDb {
        SourceDb { files: Vec::new() }
    }

    /// Register a file. Returns its id, or a diagnostic if the file is too large.
    pub fn add_file(
        &mut self,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<FileId, String> {
        let file = SourceFile::new(name, text);
        if file.len() > u32::MAX as usize {
            return Err(format!(
                "source file '{}' is larger than 4 GiB and is not supported",
                file.name()
            ));
        }
        let id = FileId(self.files.len() as u32);
        self.files.push(file);
        Ok(id)
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> impl Iterator<Item = (FileId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f))
    }

    /// The text covered by a span. Callers guarantee the span came from this db.
    pub fn span_text(&self, span: Span) -> &str {
        &self.file(span.file).text[span.start as usize..span.end as usize]
    }

    /// The full line containing `offset`, without the trailing newline.
    pub fn line_of(&self, file: FileId, offset: u32) -> &str {
        let text = &self.file(file).text;
        let offset = (offset as usize).min(text.len());
        let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = text[offset..]
            .find('\n')
            .map(|i| offset + i)
            .unwrap_or(text.len());
        &text[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> SourceDb {
        let mut db = SourceDb::new();
        db.add_file("a.v", "module m;\n  wire x;\nendmodule\n")
            .unwrap();
        db
    }

    #[test]
    fn line_col_is_one_based() {
        let db = db();
        let f = FileId(0);
        assert_eq!(db.file(f).line_col(0), (1, 1));
        assert_eq!(db.file(f).line_col(10), (2, 1)); // start of "  wire x;"
        assert_eq!(db.file(f).line_col(13), (2, 4)); // 'r' in wire
    }

    #[test]
    fn span_text_slices_source() {
        let db = db();
        let f = FileId(0);
        assert_eq!(db.span_text(Span::new(f, 7, 8)), "m");
    }

    #[test]
    fn line_of_returns_full_line() {
        let db = db();
        assert_eq!(db.line_of(FileId(0), 12), "  wire x;");
    }
}
