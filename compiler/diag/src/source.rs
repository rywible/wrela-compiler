//! Source files and byte spans.

use std::fmt;

/// A file in a [`SourceMap`]. Ids are dense and start at 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(pub u32);

/// A half-open byte range `start..end` in one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Span {
        debug_assert!(start <= end, "span {start}..{end} is reversed");
        Span { file, start, end }
    }

    /// The smallest span covering both. Both must be in the same file.
    pub fn to(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span { file: self.file, start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    /// An empty span at this span's start.
    pub fn shrink_to_start(self) -> Span {
        Span { end: self.start, ..self }
    }

    /// An empty span at this span's end.
    pub fn shrink_to_end(self) -> Span {
        Span { start: self.end, ..self }
    }

    /// The byte range, for slicing the file's text.
    pub fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}..{}", self.file.0, self.start, self.end)
    }
}

/// One source file: its display name, its text and where its lines start.
#[derive(Debug)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<u32>,
}

/// A 1-based line and column; the column counts characters, not bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceFile {
        let text = text.into();
        // A byte-order mark isn't part of the first line (L2): columns count from after it.
        let mut line_starts = vec![if text.starts_with('\u{feff}') { 3 } else { 0 }];
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => line_starts.push(i as u32 + 1),
                b'\r' if bytes.get(i + 1) != Some(&b'\n') => line_starts.push(i as u32 + 1),
                _ => {}
            }
            i += 1;
        }
        SourceFile { name: name.into(), text, line_starts }
    }

    /// The 0-based line holding byte `offset`.
    pub fn line_index(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    pub fn line_col(&self, offset: u32) -> LineCol {
        let line = self.line_index(offset);
        let start = self.line_starts[line] as usize;
        let end = (offset as usize).clamp(start, self.text.len().max(start));
        let column = self.text.get(start..end).map_or(end - start, |s| s.chars().count());
        LineCol { line: line as u32 + 1, column: column as u32 + 1 }
    }

    /// Where byte `offset` is, as diagnostics show it: `name:line:column`.
    pub fn location(&self, offset: u32) -> String {
        let lc = self.line_col(offset);
        format!("{}:{}:{}", self.name, lc.line, lc.column)
    }

    /// The text of 0-based line `line`, without its line break.
    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line] as usize;
        let end = self.line_starts.get(line + 1).map_or(self.text.len(), |&e| e as usize);
        self.text.get(start..end).unwrap_or("").trim_end_matches(['\n', '\r'])
    }

    /// The byte offset where 0-based line `line` starts.
    pub fn line_start(&self, line: usize) -> u32 {
        self.line_starts[line]
    }

    /// How many lines the file has.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The byte offset where the text of 0-based line `line` ends (before the line break).
    pub fn line_end(&self, line: usize) -> u32 {
        self.line_starts[line] + self.line_text(line).len() as u32
    }
}

/// Every file a compilation has read, by [`FileId`].
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        self.files.push(SourceFile::new(name, text));
        FileId(self.files.len() as u32 - 1)
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> impl Iterator<Item = (FileId, &SourceFile)> {
        self.files.iter().enumerate().map(|(i, f)| (FileId(i as u32), f))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_chars() {
        let f = SourceFile::new("a", "ab\nαβc\n");
        assert_eq!(f.line_col(0), LineCol { line: 1, column: 1 });
        assert_eq!(f.line_col(3), LineCol { line: 2, column: 1 });
        assert_eq!(f.line_col(7), LineCol { line: 2, column: 3 });
        assert_eq!(f.line_text(1), "αβc");
    }

    #[test]
    fn a_byte_order_mark_is_no_column() {
        let f = SourceFile::new("a", "\u{feff}fn f\nx");
        assert_eq!(f.line_col(3), LineCol { line: 1, column: 1 });
        assert_eq!(f.line_col(6), LineCol { line: 1, column: 4 });
        assert_eq!(f.line_col(0), LineCol { line: 1, column: 1 });
        assert_eq!(f.line_text(0), "fn f");
        assert_eq!(f.line_col(8), LineCol { line: 2, column: 1 });
        let empty = SourceFile::new("b", "\u{feff}");
        assert_eq!(empty.line_col(3), LineCol { line: 1, column: 1 });
        assert_eq!(empty.line_text(0), "");
    }

    #[test]
    fn crlf_is_one_break() {
        let f = SourceFile::new("a", "a\r\nb\rc");
        assert_eq!(f.line_text(0), "a");
        assert_eq!(f.line_text(1), "b");
        assert_eq!(f.line_col(5).line, 3);
    }
}
